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
	cantonListPublishesAuthAudience       *string
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
	cantonListPublishesReadAsParty = flags.String("cantonReadAsParty", "", "guardianGovernance party id to read as (<hint>::1220<64 lowercase hex>)")
	cantonListPublishesFromOffset = flags.Int64("fromOffset", -1, "Exclusive participant offset to start after (the offset recorded at step 1 of the upgrade)")
	cantonListPublishesExecutingPackageID = flags.String("executingPackageID", "", "wormhole-core package id (64 lowercase hex) whose publishes to list")
	cantonListPublishesAuthTokenURL = flags.String("cantonAuthTokenURL", "", "OAuth2 token endpoint; the client secret is read from GUARDIAND_CANTONAUTHCLIENTSECRET")
	cantonListPublishesAuthClientID = flags.String("cantonAuthClientID", "", "OAuth2 client id")
	cantonListPublishesAuthAudience = flags.String("cantonAuthAudience", "", "Optional OAuth2 audience: the Ledger API target audience of the participant")
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

// cantonPublishListing is the publishes the watcher accepts, and the count of skipped foreign-registrar publishes.
type cantonPublishListing struct {
	Records                      []cantonPublishRecord
	SkippedForeignRegistrarCount int
}

// parseListPublishesReadScope requires a guardianGovernance party: the command has no unsafe dev mode.
func parseListPublishesReadScope(readAsParty string) (cantonclient.ReadScope, error) {
	return cantonclient.ParseReadScope(readAsParty, false)
}

// listCantonPublishes reads (fromOffset, ledger end] with the watcher's
// message filter pinned to executingPackageIDHex only.
// SECURITY: canton.CheckRegistrarIsReadAsParty is the watcher's registrar check; a skipped record has no reobservation.
func listCantonPublishes(ctx context.Context, client cantonclient.CantonClient, readScope cantonclient.ReadScope, fromOffset int64, executingPackageIDHex string) (cantonPublishListing, error) {
	if err := readScope.CheckWasConstructed(); err != nil {
		return cantonPublishListing{}, err
	}
	if fromOffset < 0 {
		return cantonPublishListing{}, fmt.Errorf("--fromOffset %d is negative", fromOffset)
	}
	executingPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: executingPackageIDHex}, cantonclient.RetiringPackageConfig{}, time.Now())
	if err != nil {
		return cantonPublishListing{}, err
	}
	ledgerEnd, err := client.GetLedgerEnd(ctx)
	if err != nil {
		return cantonPublishListing{}, err
	}
	if fromOffset > ledgerEnd {
		return cantonPublishListing{}, fmt.Errorf("--fromOffset %d is after the ledger end %d", fromOffset, ledgerEnd)
	}
	events, err := client.ReadPublishesInRange(ctx, fromOffset, ledgerEnd, canton.PublishMessageTemplateID(executingPackageIDs), canton.PublishMessageChoice)
	if err != nil {
		return cantonPublishListing{}, err
	}
	listing := cantonPublishListing{Records: make([]cantonPublishRecord, 0, len(events))}
	for _, event := range events {
		if canton.CheckRegistrarIsReadAsParty(event.Message.Registrar, readScope) != nil {
			listing.SkippedForeignRegistrarCount++
			continue
		}
		emitterAddress := canton.DeriveEmitterAddress(event.Message.Registrar, event.Message.Owner, event.Message.EmitterID)
		listing.Records = append(listing.Records, cantonPublishRecord{
			ParticipantOffset: event.ParticipantOffset,
			UpdateID:          event.UpdateID.String(),
			RecordTime:        event.RecordTime.UTC().Format(time.RFC3339Nano),
			EmitterChain:      uint16(vaa.ChainIDCanton),
			EmitterAddress:    hex.EncodeToString(emitterAddress[:]),
			Sequence:          event.Message.Sequence,
			TxID:              hex.EncodeToString(event.UpdateID.TxID()),
		})
	}
	if len(listing.Records)+listing.SkippedForeignRegistrarCount != len(events) {
		panic("canton-list-publishes: listed and skipped records do not add up to the read publishes")
	}
	return listing, nil
}

// cantonListPublishesOAuthFlags are the raw OAuth inputs of canton-list-publishes.
type cantonListPublishesOAuthFlags struct {
	tokenURL     string
	clientID     string
	clientSecret string
	audience     string
}

// cantonListPublishesOAuthCredentials returns nil when every OAuth input is empty.
func cantonListPublishesOAuthCredentials(oauthFlags cantonListPublishesOAuthFlags, unsafePlaintext bool) (*cantonclient.OAuthClientCredentials, error) {
	audience, err := cantonclient.ParseOAuthAudience(oauthFlags.audience)
	if err != nil {
		return nil, fmt.Errorf("--cantonAuthAudience: %w", err)
	}
	oauthEnabled := oauthFlags.tokenURL != "" || oauthFlags.clientID != "" || oauthFlags.clientSecret != ""
	if !oauthEnabled {
		if audience.IsSet() {
			return nil, fmt.Errorf("--cantonAuthAudience requires --cantonAuthTokenURL, --cantonAuthClientID, and GUARDIAND_CANTONAUTHCLIENTSECRET")
		}
		return nil, nil
	}
	if oauthFlags.tokenURL == "" || oauthFlags.clientID == "" || oauthFlags.clientSecret == "" {
		return nil, fmt.Errorf("--cantonAuthTokenURL, --cantonAuthClientID, and GUARDIAND_CANTONAUTHCLIENTSECRET must all be set together")
	}
	if unsafePlaintext {
		return nil, fmt.Errorf("OAuth requires TLS; remove --unsafePlaintext")
	}
	return &cantonclient.OAuthClientCredentials{
		TokenURL:     oauthFlags.tokenURL,
		ClientID:     oauthFlags.clientID,
		ClientSecret: oauthFlags.clientSecret,
		Audience:     audience,
	}, nil
}

func runCantonListPublishes(cmd *cobra.Command, _ []string) error {
	readScope, err := parseListPublishesReadScope(*cantonListPublishesReadAsParty)
	if err != nil {
		return fmt.Errorf("--cantonReadAsParty: %w", err)
	}
	ctx, cancel := context.WithTimeout(cmd.Context(), cantonListPublishesTimeout)
	defer cancel()

	var transportCreds credentials.TransportCredentials
	if *cantonListPublishesPlaintext {
		transportCreds = insecure.NewCredentials()
	}
	oauthClientCredentials, err := cantonListPublishesOAuthCredentials(cantonListPublishesOAuthFlags{
		tokenURL:     *cantonListPublishesAuthTokenURL,
		clientID:     *cantonListPublishesAuthClientID,
		clientSecret: os.Getenv("GUARDIAND_CANTONAUTHCLIENTSECRET"),
		audience:     *cantonListPublishesAuthAudience,
	}, *cantonListPublishesPlaintext)
	if err != nil {
		return err
	}
	var dialOpts []grpc.DialOption
	if oauthClientCredentials != nil {
		dialOpts = append(dialOpts, cantonclient.NewOAuthDialOption(ctx, *oauthClientCredentials))
	}
	client, err := cantonclient.NewCantonGrpcClient(*cantonListPublishesRPC, readScope, zap.NewNop(), transportCreds, dialOpts...)
	if err != nil {
		return err
	}
	defer client.Close()

	listing, err := listCantonPublishes(ctx, client, readScope, *cantonListPublishesFromOffset, *cantonListPublishesExecutingPackageID)
	if err != nil {
		return err
	}
	encoder := json.NewEncoder(cmd.OutOrStdout())
	for _, record := range listing.Records {
		if err := encoder.Encode(record); err != nil {
			return err
		}
	}
	if _, err := fmt.Fprintf(cmd.ErrOrStderr(), "skipped %d publishes whose registrar is not the read-as party\n", listing.SkippedForeignRegistrarCount); err != nil {
		return err
	}
	return nil
}
