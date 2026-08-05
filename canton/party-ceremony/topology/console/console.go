// Package console implements ceremony.Topology by driving a Canton console
// (`dpm canton-console --bootstrap`) one operation at a time through the
// ops.canton dispatcher. It reuses the proven external-signing topology recipe
// from canton/devnet/guardian_governance.canton, so the party-ceremony state
// machine can create the guardian namespace and parties on a real
// synchronizer.
//
// Scope: a single hosting participant (the console's connected participant —
// `dpm canton-console` defaults to the local sandbox). Multi-participant
// hosting is future work (a gRPC Admin-API adapter, or a per-participant
// console config); the port is shaped so that swap does not touch the domain.
package console

import (
	"bufio"
	"bytes"
	"context"
	"encoding/base64"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

// Console is a ceremony.Topology backed by the Canton console.
//
// Every console op is a fresh JVM invocation (seconds), so the deterministic
// read ops — ParticipantID, Fingerprint, and Describe — are memoized: they
// return the same value for the same input, and Describe in particular is
// called once per signer over the same prepared transaction.
type Console struct {
	opsScript string   // absolute path to ops.canton
	configs   []string // optional -c config files (empty ⇒ default sandbox connection)

	mu        sync.Mutex
	pid       string
	fpCache   map[string]string
	descCache map[string]describedTx
}

// describedTx caches a decoded transaction and the hash the dispatcher
// recomputed from its body, so a cached hit still detects a tampered hash.
type describedTx struct {
	view    ceremony.TxView
	hashHex string
}

// New returns a console-backed topology adapter. opsScript is the path to
// ops.canton; configs are optional `-c` Canton config files identifying the
// participant to drive (omit for the default sandbox connection).
func New(opsScript string, configs ...string) *Console {
	return &Console{
		opsScript: opsScript,
		configs:   configs,
		fpCache:   map[string]string{},
		descCache: map[string]describedTx{},
	}
}

func b64(b []byte) string { return base64.StdEncoding.EncodeToString(b) }

// run invokes one op and returns the parsed key=value result.
func (c *Console) run(ctx context.Context, op string, env map[string]string) (map[string]string, error) {
	outFile, err := os.CreateTemp("", "gg-out-*.txt")
	if err != nil {
		return nil, fmt.Errorf("console: temp out: %w", err)
	}
	outFile.Close()
	defer os.Remove(outFile.Name())

	args := []string{"canton-console", "--no-tty"}
	for _, cfg := range c.configs {
		args = append(args, "-c", cfg)
	}
	args = append(args, "--bootstrap", c.opsScript)

	cmd := exec.CommandContext(ctx, "dpm", args...)
	cmd.Dir = os.TempDir() // avoid auto-loading stray *.canton / daml.yaml
	cmd.Env = append(os.Environ(), "GG_OP="+op, "GG_OUT="+outFile.Name())
	for k, v := range env {
		cmd.Env = append(cmd.Env, k+"="+v)
	}
	var combined bytes.Buffer
	cmd.Stdout = &combined
	cmd.Stderr = &combined
	runErr := cmd.Run()

	result, readErr := parseKV(outFile.Name())
	if runErr != nil || readErr != nil || !strings.Contains(combined.String(), "GG_DONE") {
		return nil, fmt.Errorf("console: op %q failed (run=%v read=%v):\n%s", op, runErr, readErr, tail(combined.String(), 20))
	}
	return result, nil
}

func parseKV(path string) (map[string]string, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	m := map[string]string{}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1024*1024), 4*1024*1024)
	for sc.Scan() {
		line := sc.Text()
		if i := strings.IndexByte(line, '='); i >= 0 {
			m[line[:i]] = line[i+1:]
		}
	}
	return m, sc.Err()
}

func tail(s string, n int) string {
	lines := strings.Split(strings.TrimRight(s, "\n"), "\n")
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return strings.Join(lines, "\n")
}

// ParticipantID implements ceremony.Topology (memoized).
func (c *Console) ParticipantID(ctx context.Context) (string, error) {
	c.mu.Lock()
	cached := c.pid
	c.mu.Unlock()
	if cached != "" {
		return cached, nil
	}
	r, err := c.run(ctx, "participant-id", nil)
	if err != nil {
		return "", err
	}
	c.mu.Lock()
	c.pid = r["participantId"]
	c.mu.Unlock()
	return r["participantId"], nil
}

// Fingerprint implements ceremony.Topology (memoized per key).
func (c *Console) Fingerprint(ctx context.Context, pubDER []byte) (string, error) {
	key := b64(pubDER)
	c.mu.Lock()
	cached, ok := c.fpCache[key]
	c.mu.Unlock()
	if ok {
		return cached, nil
	}
	r, err := c.run(ctx, "fingerprint", map[string]string{"GG_PUB_B64": key})
	if err != nil {
		return "", err
	}
	c.mu.Lock()
	c.fpCache[key] = r["fingerprint"]
	c.mu.Unlock()
	return r["fingerprint"], nil
}

// PrepareRootDelegation implements ceremony.Topology.
func (c *Console) PrepareRootDelegation(ctx context.Context, pubDER []byte) (ceremony.PreparedTx, error) {
	r, err := c.run(ctx, "prepare-delegation", map[string]string{"GG_PUB_B64": b64(pubDER)})
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	return ceremony.PreparedTx{TxBase64: r["txB64"], HashHex: r["hashHex"]}, nil
}

// PrepareDecentralizedNamespace implements ceremony.Topology.
func (c *Console) PrepareDecentralizedNamespace(ctx context.Context, ownerPubsDER [][]byte, threshold int) (ceremony.PreparedTx, error) {
	encoded := make([]string, len(ownerPubsDER))
	for i, k := range ownerPubsDER {
		encoded[i] = b64(k)
	}
	r, err := c.run(ctx, "prepare-dns", map[string]string{
		"GG_OWNER_PUBS_B64": strings.Join(encoded, ","),
		"GG_THRESHOLD":      strconv.Itoa(threshold),
	})
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	return ceremony.PreparedTx{TxBase64: r["txB64"], HashHex: r["hashHex"], Namespace: r["namespace"]}, nil
}

// PreparePartyHosting implements ceremony.Topology.
func (c *Console) PreparePartyHosting(ctx context.Context, spec ceremony.HostingSpec) (ceremony.PreparedTx, error) {
	hosts := make([]string, len(spec.Hosts))
	for i, h := range spec.Hosts {
		hosts[i] = h.ParticipantUID + "|" + string(h.Permission)
	}
	env := map[string]string{
		"GG_PARTY_NAME":     spec.PartyName,
		"GG_NAMESPACE":      spec.Namespace,
		"GG_HOSTS":          strings.Join(hosts, ","),
		"GG_CONF_THRESHOLD": strconv.Itoa(spec.ConfirmationThreshold),
	}
	if len(spec.SigningKeysDER) > 0 {
		encoded := make([]string, len(spec.SigningKeysDER))
		for i, k := range spec.SigningKeysDER {
			encoded[i] = b64(k)
		}
		env["GG_SIGNING_KEYS_B64"] = strings.Join(encoded, ",")
		env["GG_SIGNING_THRESHOLD"] = strconv.Itoa(spec.SigningThreshold)
	}
	r, err := c.run(ctx, "prepare-hosting", env)
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	return ceremony.PreparedTx{TxBase64: r["txB64"], HashHex: r["hashHex"], PartyID: r["partyId"]}, nil
}

// Describe implements ceremony.Topology (memoized per transaction body).
func (c *Console) Describe(ctx context.Context, tx ceremony.PreparedTx) (ceremony.TxView, error) {
	c.mu.Lock()
	cached, ok := c.descCache[tx.TxBase64]
	c.mu.Unlock()
	if ok {
		if got := cached.hashHex; got != "" && got != tx.HashHex {
			return ceremony.TxView{}, fmt.Errorf("console: transaction hash %q does not match its body hash %q", tx.HashHex, got)
		}
		return cached.view, nil
	}
	r, err := c.run(ctx, "describe", map[string]string{"GG_TX_B64": tx.TxBase64})
	if err != nil {
		return ceremony.TxView{}, err
	}
	// The dispatcher recomputes the hash from the decoded bytes; if it does not
	// match the caller's HashHex, the (body, hash) pair was tampered.
	if got := r["hashHex"]; got != tx.HashHex {
		return ceremony.TxView{}, fmt.Errorf("console: transaction hash %q does not match its body hash %q", tx.HashHex, got)
	}
	view := ceremony.TxView{SigningThreshold: 0} // threshold not exposed by the console decode
	switch r["kind"] {
	case "delegation":
		view.Kind = ceremony.DelegationTx
		view.OwnerFingerprints = splitNonEmpty(r["ownerFps"])
	case "namespace":
		view.Kind = ceremony.NamespaceTx
		view.OwnerFingerprints = splitNonEmpty(r["ownerFps"])
		view.Threshold, _ = strconv.Atoi(r["threshold"])
	case "hosting":
		view.Kind = ceremony.HostingTx
		view.PartyName = r["partyName"]
		view.SigningKeyFingerprints = splitNonEmpty(r["signingFps"])
		for _, h := range splitNonEmpty(r["hosts"]) {
			parts := strings.SplitN(h, "|", 2)
			if len(parts) == 2 {
				view.Hosts = append(view.Hosts, ceremony.Host{ParticipantUID: parts[0], Permission: ceremony.Permission(parts[1])})
			}
		}
	default:
		return ceremony.TxView{}, fmt.Errorf("console: unknown transaction kind %q", r["kind"])
	}
	c.mu.Lock()
	c.descCache[tx.TxBase64] = describedTx{view: view, hashHex: r["hashHex"]}
	c.mu.Unlock()
	return view, nil
}

func splitNonEmpty(s string) []string {
	if s == "" {
		return nil
	}
	return strings.Split(s, ",")
}

// Consent implements ceremony.Topology. In the single-participant model the
// hosting participant applies its own consent at submit time (with its vault
// key), so this returns a non-empty marker to satisfy the workflow's
// all-participants-consented gate.
func (c *Console) Consent(ctx context.Context, tx ceremony.PreparedTx) (string, error) {
	return "consent:" + tx.HashHex, nil
}

// Submit implements ceremony.Topology.
func (c *Console) Submit(ctx context.Context, tx ceremony.PreparedTx, ownerSigs []ceremony.OwnerSignature, _ []string) error {
	sigs := make([]string, 0, len(ownerSigs))
	for _, s := range ownerSigs {
		if s.Fingerprint == "" {
			return fmt.Errorf("console: owner signature for %q is missing its fingerprint", s.OwnerID)
		}
		sigs = append(sigs, s.Fingerprint+":"+s.SigHex)
	}
	_, err := c.run(ctx, "submit", map[string]string{
		"GG_TX_B64":     tx.TxBase64,
		"GG_OWNER_SIGS": strings.Join(sigs, ","),
	})
	return err
}

// PartyPresent reports whether a PartyToParticipant mapping for partyID is on
// the synchronizer — a verification helper for tests, not part of the port.
func (c *Console) PartyPresent(ctx context.Context, partyID string) (bool, error) {
	r, err := c.run(ctx, "read-party", map[string]string{"GG_PARTY_ID": partyID})
	if err != nil {
		return false, err
	}
	return r["present"] == "true", nil
}
