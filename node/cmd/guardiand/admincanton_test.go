package guardiand

import (
	"context"
	"encoding/hex"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/watchers/canton"
)

const (
	listedPackageIDHex = "1111111111111111111111111111111111111111111111111111111111111111"
	otherPackageIDHex  = "2222222222222222222222222222222222222222222222222222222222222222"
	listedUpdateIDHex  = "1220ac5126912ade361264f7d40c4711a72dc9b3d12e9ec044a07d5df586b2fe397b"
	listedRegistrar    = "vector-operator::1220deadbeef"
	listedOwner        = "vector-owner::1220cafebabe"
)

// rangePublish is a publish of the fake ledger with the package id that executed it.
type rangePublish struct {
	executingPackageIDHex string
	event                 cantonclient.CantonMessageEvent
}

// fakeRangeClient serves ReadPublishesInRange from rangePublishes, filtered by the template like the gRPC client.
type fakeRangeClient struct {
	cantonclient.CantonClient
	ledgerEnd      int64
	ledgerEndErr   error
	rangePublishes []rangePublish
	rangeRequests  [][2]int64
}

func (f *fakeRangeClient) GetLedgerEnd(context.Context) (int64, error) {
	return f.ledgerEnd, f.ledgerEndErr
}

func (f *fakeRangeClient) ReadPublishesInRange(_ context.Context, beginExclusive int64, endInclusive int64, tmpl cantonclient.TemplateID, choiceName string) ([]cantonclient.CantonMessageEvent, error) {
	f.rangeRequests = append(f.rangeRequests, [2]int64{beginExclusive, endInclusive})
	if choiceName != "PublishMessage" || tmpl.ModuleName != "Wormhole.Core.State" || tmpl.EntityName != "Emitter" {
		return nil, errors.New("unexpected template filter")
	}
	events := []cantonclient.CantonMessageEvent{}
	for _, publish := range f.rangePublishes {
		inRange := publish.event.ParticipantOffset > beginExclusive && publish.event.ParticipantOffset <= endInclusive
		verdict := tmpl.AcceptedPackageIDs.VerdictForPublish(publish.executingPackageIDHex, publish.event.RecordTime)
		if _, accepted := verdict.AcceptedRole(); inRange && accepted {
			events = append(events, publish.event)
		}
	}
	return events, nil
}

func listedPublish(executingPackageIDHex string, participantOffset int64, sequence uint64) rangePublish {
	updateID, err := cantonclient.ParseUpdateID(listedUpdateIDHex)
	if err != nil {
		panic(err)
	}
	return rangePublish{
		executingPackageIDHex: executingPackageIDHex,
		event: cantonclient.CantonMessageEvent{
			ParticipantOffset: participantOffset,
			UpdateID:          updateID,
			EffectiveAt:       time.Date(2026, 11, 17, 9, 0, 0, 0, time.UTC),
			RecordTime:        time.Date(2026, 11, 17, 9, 0, 1, 0, time.UTC),
			Message: cantonclient.CantonMessage{
				Registrar:           listedRegistrar,
				Owner:               listedOwner,
				EmitterID:           7,
				Sequence:            sequence,
				AcceptedPackageRole: cantonclient.AcceptedPackageRoleCurrent,
			},
		},
	}
}

func TestListPublishesFiltersByExecutingPackage(t *testing.T) {
	ledger := []rangePublish{
		listedPublish(listedPackageIDHex, 5, 1),
		listedPublish(otherPackageIDHex, 6, 2),
		listedPublish(listedPackageIDHex, 9, 3),
		listedPublish(listedPackageIDHex, 21, 4),
	}
	updateID, err := cantonclient.ParseUpdateID(listedUpdateIDHex)
	require.NoError(t, err)
	wantEmitterAddress := canton.DeriveEmitterAddress(listedRegistrar, listedOwner, 7)
	cases := []struct {
		name               string
		executingPackageID string
		fromOffset         int64
		ledgerEnd          int64
		ledgerEndErr       error
		wantSequences      []uint64
		wantRange          [2]int64
		wantErr            string
	}{
		{name: "matching id", executingPackageID: listedPackageIDHex, fromOffset: 0, ledgerEnd: 20, wantSequences: []uint64{1, 3}, wantRange: [2]int64{0, 20}},
		{name: "other id", executingPackageID: otherPackageIDHex, fromOffset: 0, ledgerEnd: 20, wantSequences: []uint64{2}, wantRange: [2]int64{0, 20}},
		{name: "from offset excludes earlier publishes", executingPackageID: listedPackageIDHex, fromOffset: 5, ledgerEnd: 20, wantSequences: []uint64{3}, wantRange: [2]int64{5, 20}},
		{name: "empty range", executingPackageID: listedPackageIDHex, fromOffset: 20, ledgerEnd: 20, wantSequences: []uint64{}, wantRange: [2]int64{20, 20}},
		{name: "from offset after ledger end", executingPackageID: listedPackageIDHex, fromOffset: 21, ledgerEnd: 20, wantErr: "after the ledger end"},
		{name: "negative from offset", executingPackageID: listedPackageIDHex, fromOffset: -1, ledgerEnd: 20, wantErr: "negative"},
		{name: "malformed package id", executingPackageID: "pkg", fromOffset: 0, ledgerEnd: 20, wantErr: "64 lowercase hex"},
		{name: "ledger end error", executingPackageID: listedPackageIDHex, fromOffset: 0, ledgerEndErr: errors.New("unavailable"), wantErr: "unavailable"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client := &fakeRangeClient{ledgerEnd: tc.ledgerEnd, ledgerEndErr: tc.ledgerEndErr, rangePublishes: ledger}

			records, err := listCantonPublishes(context.Background(), client, tc.fromOffset, tc.executingPackageID)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Nil(t, records)
				assert.Empty(t, client.rangeRequests)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, [][2]int64{tc.wantRange}, client.rangeRequests)
			sequences := make([]uint64, 0, len(records))
			for _, record := range records {
				sequences = append(sequences, record.Sequence)
				assert.Equal(t, uint16(vaa.ChainIDCanton), record.EmitterChain)
				assert.Equal(t, hex.EncodeToString(wantEmitterAddress[:]), record.EmitterAddress)
				assert.Equal(t, listedUpdateIDHex, record.UpdateID)
				assert.Equal(t, hex.EncodeToString(updateID.TxID()), record.TxID)
				assert.Equal(t, "2026-11-17T09:00:01Z", record.RecordTime)
			}
			assert.Equal(t, tc.wantSequences, sequences)
		})
	}
}
