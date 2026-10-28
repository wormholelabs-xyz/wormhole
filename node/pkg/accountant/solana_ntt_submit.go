// NTT route resolution for the Solana NTT submission worker. The NTT program keys balances
// on the sender's hub and checks the peer pair. Thus each observation needs the hub and the
// source peer, read from chain, before its account list is complete.

package accountant

import (
	"context"
	"errors"
	"fmt"

	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"

	"go.uber.org/zap"
)

// resolveSolanaNttRoutes reads the hub and source-peer PDA of each observation in one
// batched query and fills the balance slots and slot 13. An observation whose route is
// absent or malformed is dropped as unroutable and left to the audit, as the wormchain
// contract rejects it.
//
// The read uses confirmed commitment, the same as preflight.
func (acct *Accountant) resolveSolanaNttRoutes(ctx context.Context, b *solanaBackend, work []*solanaSubmission) []*solanaSubmission {
	if len(work) == 0 {
		return nil
	}

	routed := make([]*solanaSubmission, 0, len(work))
	addrs := make([]solana.PublicKey, 0, 2*len(work))
	for _, sub := range work {
		if sub.nttRoute == nil {
			b.metrics.submitFailures.Inc()
			acct.logger.Error("skipping a solana NTT observation without route accounts", zap.String("backend", b.tag), zap.String("msgId", sub.msgId))
			continue
		}
		routed = append(routed, sub)
		addrs = append(addrs, sub.nttRoute.hubPDA, sub.nttRoute.peerSrcPDA)
	}
	if len(routed) == 0 {
		return nil
	}

	accounts, err := b.conn.GetOwnedAccounts(ctx, addrs, b.program, solacctconn.CommitmentConfirmed)
	if err != nil {
		b.metrics.submitFailures.Add(float64(len(routed)))
		acct.logger.Error("failed to read the solana NTT route accounts", zap.String("backend", b.tag), zap.Int("numMsgs", len(routed)), zap.Error(err))
		return nil
	}
	if len(accounts) != len(addrs) {
		b.metrics.submitFailures.Add(float64(len(routed)))
		acct.logger.Error("the solana NTT route read returned the wrong number of results", zap.String("backend", b.tag), zap.Int("want", len(addrs)), zap.Int("got", len(accounts)))
		return nil
	}

	ready := make([]*solanaSubmission, 0, len(routed))
	for idx, sub := range routed {
		if err := b.resolveSolanaNttRoute(sub, accounts[2*idx], accounts[2*idx+1]); err != nil {
			b.metrics.submitFailures.Inc()
			acct.logger.Error("skipping an unroutable solana NTT observation, the audit will retry", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.String("reason", "unroutable"), zap.Error(err))
			continue
		}
		ready = append(ready, sub)
	}
	return ready
}

// resolveSolanaNttRoute decodes the hub and source-peer accounts of one observation, then
// derives its hub balances and the destination peer PDA.
//
// SECURITY: postcondition on success: sub.nttRoute.resolved, and the balance slots and slot
// 13 come from the decoded hub and peer accounts.
func (b *solanaBackend) resolveSolanaNttRoute(sub *solanaSubmission, hubAccount solacctconn.OwnedAccount, peerSrcAccount solacctconn.OwnedAccount) error {
	fields, ok := sub.record.(*solanaNttObservationFields)
	if !ok || fields == nil {
		return fmt.Errorf("want an NTT record, got %T", sub.record)
	}
	if sub.nttRoute == nil {
		return errors.New("no NTT route accounts")
	}
	if hubAccount.State != solacctconn.AccountInitialised {
		return errors.New("the sender has no transceiver hub")
	}
	if peerSrcAccount.State != solacctconn.AccountInitialised {
		return errors.New("the sender has no peer on the recipient chain")
	}

	hub, err := parseTransceiverHubAccount(hubAccount.Data, fields.Chain, fields.Sender)
	if err != nil {
		return err
	}
	peer, err := parseTransceiverPeerAccount(peerSrcAccount.Data, fields.Chain, fields.Sender, fields.RecipientChain)
	if err != nil {
		return err
	}

	if sub.sourceBalance, err = deriveBalanceAccountPDA(b.program, fields.Chain, hub.Chain, hub.Address); err != nil {
		return err
	}
	if sub.destBalance, err = deriveBalanceAccountPDA(b.program, fields.RecipientChain, hub.Chain, hub.Address); err != nil {
		return err
	}
	if sub.nttRoute.peerDstPDA, err = deriveTransceiverPeerPDA(b.program, fields.RecipientChain, peer, fields.Chain); err != nil {
		return err
	}
	sub.nttRoute.resolved = true
	return nil
}
