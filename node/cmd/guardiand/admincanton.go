package guardiand

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/spf13/cobra"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/watchers/canton"
)

// cantonListPublishesTimeout bounds one canton-list-publishes run.
const cantonListPublishesTimeout = 10 * time.Minute

var (
	cantonListPublishesRPC                *string
	cantonListPublishesReadAsParty        *string
	cantonListPublishesFromOffset         *int64
	cantonListPublishesExecutingPackageID *string
	cantonListPublishesAuthTokenURL       *string
	cantonListPublishesAuthClientID       *string
	cantonListPublishesPlaintext          *bool
)

// AdminClientCantonListPublishesCmd lists the PublishMessage exercises of one
// core package id in (fromOffset, ledger end] for old-core message recovery
// (canton/README.md section 7.5).
var AdminClientCantonListPublishesCmd = &cobra.Command{
	Use:   "canton-list-publishes",
	Short: "List PublishMessage exercises of one wormhole-core package id after an offset, as JSON lines",
	RunE:  runCantonListPublishes,
	Args:  cobra.NoArgs,
}

func init() {
	flags := AdminClientCantonListPublishesCmd.Flags()
	cantonListPublishesRPC = flags.String("cantonRPC", "", "Canton Ledger API v2 gRPC endpoint (host:port)")
	cantonListPublishesReadAsParty = flags.String("cantonReadAsParty", "", "guardianGovernance party to read as")
	cantonListPublishesFromOffset = flags.Int64("fromOffset", -1, "Exclusive participant offset to start after (the offset recorded at step 1 of the upgrade)")
	cantonListPublishesExecutingPackageID = flags.String("executingPackageID", "", "wormhole-core package id (64 lowercase hex) whose publishes to list")
	cantonListPublishesAuthTokenURL = flags.String("cantonAuthTokenURL", "", "OAuth2 token endpoint; the client secret is read from GUARDIAND_CANTONAUTHCLIENTSECRET")
	cantonListPublishesAuthClientID = flags.String("cantonAuthClientID", "", "OAuth2 client id")
	cantonListPublishesPlaintext = flags.Bool("unsafePlaintext", false, "Dial without TLS (local sandbox only)")
	for _, requiredFlag := range []string{"cantonRPC", "cantonReadAsParty", "fromOffset", "executingPackageID"} {
		if err := AdminClientCantonListPublishesCmd.MarkFlagRequired(requiredFlag); err != nil {
			panic(err)
		}
	}
	AdminCmd.AddCommand(AdminClientCantonListPublishesCmd)
}

// cantonPublishRecord is one output line: what an operator needs to find the VAA and request reobservation.
type cantonPublishRecord struct {
	ParticipantOffset int64  `json:"participantOffset"`
	UpdateID          string `json:"updateId"`
	RecordTime        string `json:"recordTime"`
	EmitterChain      uint16 `json:"emitterChain"`
	EmitterAddress    string `json:"emitterAddress"`
	Sequence          uint64 `json:"sequence"`
	TxID              string `json:"txId"`
}

// listCantonPublishes reads (fromOffset, ledger end] with the watcher's
// message filter pinned to executingPackageIDHex only.
func listCantonPublishes(ctx context.Context, client cantonclient.CantonClient, fromOffset int64, executingPackageIDHex string) ([]cantonPublishRecord, error) {
	if fromOffset < 0 {
		return nil, fmt.Errorf("--fromOffset %d is negative", fromOffset)
	}
	executingPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: executingPackageIDHex}, cantonclient.RetiringPackageConfig{}, time.Now())
	if err != nil {
		return nil, err
	}
	ledgerEnd, err := client.GetLedgerEnd(ctx)
	if err != nil {
		return nil, err
	}
	if fromOffset > ledgerEnd {
		return nil, fmt.Errorf("--fromOffset %d is after the ledger end %d", fromOffset, ledgerEnd)
	}
	events, err := client.ReadPublishesInRange(ctx, fromOffset, ledgerEnd, canton.PublishMessageTemplateID(executingPackageIDs), canton.PublishMessageChoice)
	if err != nil {
		return nil, err
	}
	records := make([]cantonPublishRecord, 0, len(events))
	for _, event := range events {
		emitterAddress := canton.DeriveEmitterAddress(event.Message.Registrar, event.Message.Owner, event.Message.EmitterID)
		records = append(records, cantonPublishRecord{
			ParticipantOffset: event.ParticipantOffset,
			UpdateID:          event.UpdateID.String(),
			RecordTime:        event.RecordTime.UTC().Format(time.RFC3339Nano),
			EmitterChain:      uint16(vaa.ChainIDCanton),
			EmitterAddress:    hex.EncodeToString(emitterAddress[:]),
			Sequence:          event.Message.Sequence,
			TxID:              hex.EncodeToString(event.UpdateID.TxID()),
		})
	}
	return records, nil
}

func runCantonListPublishes(cmd *cobra.Command, _ []string) error {
	ctx, cancel := context.WithTimeout(cmd.Context(), cantonListPublishesTimeout)
	defer cancel()

	var transportCreds credentials.TransportCredentials
	if *cantonListPublishesPlaintext {
		transportCreds = insecure.NewCredentials()
	}
	var dialOpts []grpc.DialOption
	if *cantonListPublishesAuthTokenURL != "" || *cantonListPublishesAuthClientID != "" {
		clientSecret := os.Getenv("GUARDIAND_CANTONAUTHCLIENTSECRET")
		if *cantonListPublishesAuthTokenURL == "" || *cantonListPublishesAuthClientID == "" || clientSecret == "" {
			return fmt.Errorf("--cantonAuthTokenURL, --cantonAuthClientID, and GUARDIAND_CANTONAUTHCLIENTSECRET must all be set together")
		}
		if *cantonListPublishesPlaintext {
			return fmt.Errorf("OAuth requires TLS; remove --unsafePlaintext")
		}
		dialOpts = append(dialOpts, cantonclient.NewOAuthDialOption(ctx, *cantonListPublishesAuthTokenURL, *cantonListPublishesAuthClientID, clientSecret))
	}
	client, err := cantonclient.NewCantonGrpcClient(*cantonListPublishesRPC, *cantonListPublishesReadAsParty, zap.NewNop(), transportCreds, dialOpts...)
	if err != nil {
		return err
	}
	defer client.Close()

	records, err := listCantonPublishes(ctx, client, *cantonListPublishesFromOffset, *cantonListPublishesExecutingPackageID)
	if err != nil {
		return err
	}
	encoder := json.NewEncoder(cmd.OutOrStdout())
	for _, record := range records {
		if err := encoder.Encode(record); err != nil {
			return err
		}
	}
	return nil
}
