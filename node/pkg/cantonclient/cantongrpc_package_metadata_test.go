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

func TestListVettedPackageMetadataPaginates(t *testing.T) {
	currentPackageID, err := ParsePackageID(currentPackageIDHex)
	require.NoError(t, err)
	retiringPackageID, err := ParsePackageID(retiringPackageIDHex)
	require.NoError(t, err)
	twoPages := map[string]*apiv2.ListVettedPackagesResponse{
		"":       vettedPage("page-2", &apiv2.VettedPackage{PackageId: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"}),
		"page-2": vettedPage("", &apiv2.VettedPackage{PackageId: retiringPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.0"}),
	}
	endlessPages := map[string]*apiv2.ListVettedPackagesResponse{}
	for pageIndex := 0; pageIndex <= maxVettedPackagePages; pageIndex++ {
		token := ""
		if pageIndex > 0 {
			token = fmt.Sprintf("page-%d", pageIndex)
		}
		endlessPages[token] = vettedPage(fmt.Sprintf("page-%d", pageIndex+1))
	}
	cases := []struct {
		name        string
		service     *fakePackageService
		wantEntries []VettedPackageMetadata
		wantPages   int
		wantErr     string
	}{
		{
			name:    "two pages",
			service: &fakePackageService{pagesByToken: twoPages},
			wantEntries: []VettedPackageMetadata{
				{PackageID: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"},
				{PackageID: retiringPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.0"},
			},
			wantPages: 2,
		},
		{name: "page cap exceeded", service: &fakePackageService{pagesByToken: endlessPages}, wantPages: maxVettedPackagePages, wantErr: "more than"},
		{name: "rpc error", service: &fakePackageService{err: errors.New("unavailable")}, wantPages: 1, wantErr: "unavailable"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client := &grpcClient{packages: tc.service, logger: zap.NewNop()}

			entries, err := client.ListVettedPackageMetadata(context.Background(), []PackageID{currentPackageID, retiringPackageID})

			assert.Len(t, tc.service.requests, tc.wantPages)
			for _, request := range tc.service.requests {
				assert.Equal(t, []string{currentPackageIDHex, retiringPackageIDHex}, request.GetPackageMetadataFilter().GetPackageIds())
				assert.Empty(t, request.GetPackageMetadataFilter().GetPackageNamePrefixes())
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

func TestListVettedPackageMetadataRequiresPackageIDs(t *testing.T) {
	service := &fakePackageService{}
	client := &grpcClient{packages: service, logger: zap.NewNop()}

	_, err := client.ListVettedPackageMetadata(context.Background(), nil)

	require.ErrorContains(t, err, "at least one package id")
	assert.Empty(t, service.requests, "an empty filter matches every vetted package")
}
