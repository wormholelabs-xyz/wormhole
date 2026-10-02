package accountant

import (
	"github.com/prometheus/client_golang/prometheus/promauto"

	"github.com/prometheus/client_golang/prometheus"
)

var (
	transfersOutstanding = promauto.NewGauge(
		prometheus.GaugeOpts{
			Name: "global_accountant_transfer_vaas_outstanding",
			Help: "Current number of accountant transfers vaas in the pending state",
		})
	transfersSubmitted = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_transfer_vaas_submitted",
			Help: "Total number of accountant transfer vaas submitted",
		})
	transfersApproved = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_transfer_vaas_submitted_and_approved",
			Help: "Total number of accountant transfer vaas that were submitted and approved",
		})
	eventsReceived = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_events_received",
			Help: "Total number of accountant events received from the smart contract",
		})
	errorEventsReceived = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_error_events_received",
			Help: "Total number of accountant error events received from the smart contract",
		})
	submitFailures = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_submit_failures",
			Help: "Total number of accountant transfer vaas submit failures",
		})
	balanceErrors = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_total_balance_errors",
			Help: "Total number of balance errors detected by accountant",
		})
	digestMismatches = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_total_digest_mismatches",
			Help: "Total number of digest mismatches on accountant",
		})
	connectionErrors = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_connection_errors_total",
			Help: "Total number of connection errors on accountant",
		})
	auditErrors = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_audit_errors_total",
			Help: "Total number of audit errors detected by accountant",
		})
	channelSubmitTimeouts = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_channel_submit_timeouts",
			Help: "Total number of channel submit timeouts during audit",
		})
)

// Solana accountant counters. digestMismatches and transfersOutstanding stay shared with
// the wormchain backend. The other counters are separate, so operators can tell the
// backends apart during a dual run.
var (
	solanaEventsReceived = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_events_received",
			Help: "Total number of solana accountant log events received",
		})
	solanaTransfersApproved = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_transfer_vaas_approved",
			Help: "Total number of transfer vaas approved by the solana accountant",
		})
	solanaTransfersSubmitted = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_transfer_vaas_submitted",
			Help: "Total number of transfer vaas submitted to the solana accountant",
		})
	solanaSubmitFailures = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_submit_failures",
			Help: "Total number of solana accountant submit failures",
		})
	solanaFeePayerErrors = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_fee_payer_errors",
			Help: "Total number of solana accountant fee payer errors",
		})
	solanaFeePayerLamports = promauto.NewGauge(
		prometheus.GaugeOpts{
			Name: "global_accountant_solana_fee_payer_lamports",
			Help: "Balance of the solana accountant fee payer, refreshed each audit cycle",
		})
	solanaMalformedLogs = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_malformed_logs_total",
			Help: "Total number of solana accountant transactions with malformed logs",
		})
	solanaFailedTxSkipped = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_failed_tx_skipped_total",
			Help: "Total number of failed solana transactions skipped by the accountant watcher",
		})
	solanaConnectionErrors = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_connection_errors_total",
			Help: "Total number of solana accountant connection errors",
		})
	solanaAuditErrors = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "global_accountant_solana_audit_errors_total",
			Help: "Total number of audit errors detected by the solana accountant",
		})
)
