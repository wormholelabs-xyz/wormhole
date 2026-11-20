package cantonclient

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// fakePackageService returns one scripted page per call, keyed by page token.
type fakePackageService struct {
	apiv2.PackageServiceClient
	pagesByToken map[string]*apiv2.ListVettedPackagesResponse
	err          error
	requests     []*apiv2.ListVettedPackagesRequest
}

func (f *fakePackageService) ListVettedPackages(_ context.Context, request *apiv2.ListVettedPackagesRequest, _ ...grpc.CallOption) (*apiv2.ListVettedPackagesResponse, error) {
	f.requests = append(f.requests, request)
	if f.err != nil {
		return nil, f.err
	}
	page, found := f.pagesByToken[request.GetPageToken()]
	if !found {
		return nil, fmt.Errorf("unknown page token %q", request.GetPageToken())
	}
	return page, nil
}

func vettedPage(nextPageToken string, packages ...*apiv2.VettedPackage) *apiv2.ListVettedPackagesResponse {
	return &apiv2.ListVettedPackagesResponse{
		VettedPackages: []*apiv2.VettedPackages{{Packages: packages, ParticipantId: "participant1::1220", SynchronizerId: "synchronizer::1220"}},
		NextPageToken:  nextPageToken,
	}
}

// fakeVersionService reports maxVettedPackagesPageSize as the server maximum page size of ListVettedPackages.
type fakeVersionService struct {
	apiv2.VersionServiceClient
	maxVettedPackagesPageSize int32
	err                       error
	requestCount              int
}

func (f *fakeVersionService) GetLedgerApiVersion(_ context.Context, _ *apiv2.GetLedgerApiVersionRequest, _ ...grpc.CallOption) (*apiv2.GetLedgerApiVersionResponse, error) {
	f.requestCount++
	if f.err != nil {
		return nil, f.err
	}
	return &apiv2.GetLedgerApiVersionResponse{
		Version:  "3.7.3",
		Features: &apiv2.FeaturesDescriptor{PackageFeature: &apiv2.PackageFeature{MaxVettedPackagesPageSize: f.maxVettedPackagesPageSize}},
	}, nil
}

// chainedPages returns emptyPageCount pages without packages, each with a next-page token, then lastPage.
func chainedPages(emptyPageCount int, lastPage *apiv2.ListVettedPackagesResponse) map[string]*apiv2.ListVettedPackagesResponse {
	pagesByToken := make(map[string]*apiv2.ListVettedPackagesResponse, emptyPageCount+1)
	token := ""
	for pageIndex := 0; pageIndex < emptyPageCount; pageIndex++ {
		nextToken := fmt.Sprintf("page-%d", pageIndex+1)
		pagesByToken[token] = &apiv2.ListVettedPackagesResponse{NextPageToken: nextToken}
		token = nextToken
	}
	pagesByToken[token] = lastPage
	return pagesByToken
}

func TestListVettedPackageMetadataPaginates(t *testing.T) {
	currentPackageID, err := ParsePackageID(currentPackageIDHex)
	require.NoError(t, err)
	retiringPackageID, err := ParsePackageID(retiringPackageIDHex)
	require.NoError(t, err)
	const serverMaxPageSize = 1000
	serverMaxPageCount := maxVettedPackagePagesForPageSize(serverMaxPageSize)
	twoPages := map[string]*apiv2.ListVettedPackagesResponse{
		"":       vettedPage("page-2", &apiv2.VettedPackage{PackageId: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"}),
		"page-2": vettedPage("", &apiv2.VettedPackage{PackageId: retiringPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.0"}),
	}
	currentAfterEmptyPages := chainedPages(20, vettedPage("", &apiv2.VettedPackage{PackageId: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"}))
	cases := []struct {
		name             string
		service          *fakePackageService
		versionService   *fakeVersionService
		wantEntries      []VettedPackageMetadata
		wantPages        int
		wantPageSize     uint32
		wantVersionCalls int
		wantErr          string
	}{
		{
			name:           "two pages",
			service:        &fakePackageService{pagesByToken: twoPages},
			versionService: &fakeVersionService{maxVettedPackagesPageSize: serverMaxPageSize},
			wantEntries: []VettedPackageMetadata{
				{PackageID: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"},
				{PackageID: retiringPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.0"},
			},
			wantPages:        2,
			wantPageSize:     serverMaxPageSize,
			wantVersionCalls: 1,
		},
		{
			name:             "empty pages with a next-page token before the match",
			service:          &fakePackageService{pagesByToken: currentAfterEmptyPages},
			versionService:   &fakeVersionService{maxVettedPackagesPageSize: serverMaxPageSize},
			wantEntries:      []VettedPackageMetadata{{PackageID: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"}},
			wantPages:        21,
			wantPageSize:     serverMaxPageSize,
			wantVersionCalls: 1,
		},
		{
			name:             "page cap exceeded",
			service:          &fakePackageService{pagesByToken: chainedPages(serverMaxPageCount, vettedPage(""))},
			versionService:   &fakeVersionService{maxVettedPackagesPageSize: serverMaxPageSize},
			wantPages:        serverMaxPageCount,
			wantPageSize:     serverMaxPageSize,
			wantVersionCalls: 1,
			wantErr:          "more than",
		},
		{
			name:             "rpc error",
			service:          &fakePackageService{err: errors.New("unavailable")},
			versionService:   &fakeVersionService{maxVettedPackagesPageSize: serverMaxPageSize},
			wantPages:        1,
			wantPageSize:     serverMaxPageSize,
			wantVersionCalls: 1,
			wantErr:          "unavailable",
		},
		{
			name:             "version rpc error",
			service:          &fakePackageService{pagesByToken: twoPages},
			versionService:   &fakeVersionService{err: errors.New("version unavailable")},
			wantVersionCalls: 1,
			wantErr:          "version unavailable",
		},
		{
			name:             "server reports no maximum page size",
			service:          &fakePackageService{pagesByToken: twoPages},
			versionService:   &fakeVersionService{maxVettedPackagesPageSize: 0},
			wantVersionCalls: 1,
			wantErr:          "maximum page size 0",
		},
		{
			name:             "server reports a negative maximum page size",
			service:          &fakePackageService{pagesByToken: twoPages},
			versionService:   &fakeVersionService{maxVettedPackagesPageSize: -1},
			wantVersionCalls: 1,
			wantErr:          "maximum page size -1",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client := &grpcClient{packages: tc.service, version: tc.versionService, logger: zap.NewNop()}

			entries, err := client.ListVettedPackageMetadata(context.Background(), []PackageID{currentPackageID, retiringPackageID})

			assert.Equal(t, tc.wantVersionCalls, tc.versionService.requestCount)
			assert.Len(t, tc.service.requests, tc.wantPages)
			for _, request := range tc.service.requests {
				assert.Equal(t, []string{currentPackageIDHex, retiringPackageIDHex}, request.GetPackageMetadataFilter().GetPackageIds())
				assert.Empty(t, request.GetPackageMetadataFilter().GetPackageNamePrefixes())
				assert.Equal(t, tc.wantPageSize, request.GetPageSize())
			}
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Nil(t, entries)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantEntries, entries)
		})
	}
}

func TestMaxVettedPackagePagesForPageSize(t *testing.T) {
	cases := []struct {
		pageSize      int
		wantPageCount int
	}{
		{pageSize: 1, wantPageCount: maxScannedVettedPackagesEntries + 1},
		{pageSize: 1000, wantPageCount: maxScannedVettedPackagesEntries/1000 + 2},
		{pageSize: maxScannedVettedPackagesEntries, wantPageCount: 2},
		{pageSize: maxScannedVettedPackagesEntries + 1, wantPageCount: 2},
	}
	for _, tc := range cases {
		t.Run(fmt.Sprintf("page size %d", tc.pageSize), func(t *testing.T) {
			assert.Equal(t, tc.wantPageCount, maxVettedPackagePagesForPageSize(tc.pageSize))
		})
	}
}

func TestListVettedPackageMetadataRequiresPackageIDs(t *testing.T) {
	service := &fakePackageService{}
	client := &grpcClient{packages: service, logger: zap.NewNop()}

	_, err := client.ListVettedPackageMetadata(context.Background(), nil)

	require.ErrorContains(t, err, "at least one package id")
	assert.Empty(t, service.requests, "an empty filter matches every vetted package")
}
