//go:build integration

package canton

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// sandboxLedgerAPIVersion calls VersionService.GetLedgerApiVersion.
func sandboxLedgerAPIVersion(t *testing.T, port string) string {
	t.Helper()
	conn, err := grpc.NewClient("localhost:"+port, grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer conn.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	response, err := apiv2.NewVersionServiceClient(conn).GetLedgerApiVersion(ctx, &apiv2.GetLedgerApiVersionRequest{})
	require.NoError(t, err)
	return response.GetVersion()
}

// checkSandboxRunsPinnedSDKVersion fails the test when the sandbox runs another SDK than daml.yaml pins.
func checkSandboxRunsPinnedSDKVersion(t *testing.T, cantonDir, port string) {
	t.Helper()
	pinnedVersion := pinnedDamlSDKVersion(t, cantonDir)
	runningVersion := sandboxLedgerAPIVersion(t, port)
	require.NotEmpty(t, runningVersion)
	require.Equalf(t, pinnedVersion, runningVersion, "sandbox Ledger API version %s, daml.yaml pins SDK %s", runningVersion, pinnedVersion)
}
