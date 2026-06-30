//! The self-describing usage document served at `GET /` (and `GET /usage`).
//!
//! This is the **complete agent-facing contract**: point a client at the base
//! URL, `GET /`, and follow it — no external docs needed. It covers the
//! transport, the account/network/funding/permission model, the full method
//! catalogue, and every error code. Hand-authored so it stays the single source
//! of truth alongside the dispatch handlers (`dispatch.rs`). Contains no secrets
//! and needs no auth (it's discovery).
//!
//! Keep this in sync when adding/changing a method. `tests/server.rs` guards its
//! shape.

use serde_json::{json, Value};

/// Build the usage document. `api_version` / `spec_versions` come from
/// `ServerState`.
pub fn usage_doc(api_version: &str, spec_versions: &[String]) -> Value {
    json!({
        "service": "strkd — Starknet wallet companion",
        "version": env!("CARGO_PKG_VERSION"),
        "wallet_api_version": api_version,
        "supported_specs": spec_versions,
        "summary": "A local wallet that holds Starknet accounts and signs on your behalf. \
By default every sensitive action (signing, funding, account creation) pops a confirmation on \
the user's screen and only proceeds if the human approves. Private keys never leave the wallet \
— you get signatures, signed transactions, and (when a node is configured) on-chain broadcasts, \
never keys. It also PROVES SNIP-36 transactions on-device (sign + prove a private virtual tx in \
one call, then broadcast the verifier tx) — see concepts.proving + snip36_proving.",

        "alpha_notice": {
            "status": "strkd is ALPHA software under active development. Expect missing methods, \
rough edges, partial features, and capabilities that simply do not exist yet.",
            "do_not_work_around": "If you hit a limitation — a missing method, a confusing or \
unexpected error, a parameter that isn't accepted, or behaviour that blocks your task — DO NOT \
invent a workaround, fake or guess a result, fall back to handling keys/seeds/signing yourself, \
or silently abandon the step. A workaround hides the gap and makes it permanent. STOP and report \
it instead. The maintainer reads these reports and fixes strkd, so a clear report is the FASTEST \
path to the capability you want.",
            "call_to_action": "Tell your operator (the human running strkd) exactly what you need, \
using the report format below. Emit the filled-in block VERBATIM in your reply to them — don't \
just mention it in passing. They relay it to the strkd maintainer.",
            "github_shortcut": "Faster path when your operator can reach the repo: call \
companion_reportIssue with these same fields and you get back a PREFILLED GitHub new-issue `url`. \
Hand them the url — they open it, review/edit, and submit in their own browser. strkd files nothing \
itself and holds no GitHub token. The STRKD-FEEDBACK block below is the universal fallback when \
they can't (or don't want to) use GitHub.",
            "report_format": "Copy this block, fill every field, and hand it to your operator verbatim:\n\n\
=== STRKD-FEEDBACK ===\n\
goal:        <the task you were trying to accomplish, one line>\n\
attempted:   <method(s) + params you called — or \"no method exists for this\">\n\
observed:    <the exact JSON-RPC error code + message, or \"capability missing\">\n\
limitation:  <what strkd could not do, stated plainly>\n\
needed:      <the specific method / param / behaviour that would unblock you>\n\
impact:      <blocked | degraded | cosmetic> — <what you could NOT do as a result>\n\
workaround_avoided: <what you were tempted to do instead, so we can design it away; \"none\" if N/A>\n\
context:     strkd v<version> · network <from companion_getStatus> · <UTC timestamp>\n\
=== END ===",
            "guidance": [
                "One report per distinct limitation — don't batch unrelated gaps into one block.",
                "Be concrete in `needed`: 'a companion_getBalance {address} method' beats 'better balance support'.",
                "Fill `context` from companion_getStatus (version + network) so the maintainer can reproduce.",
                "If a call errored, paste the literal JSON-RPC error object into `observed` — don't paraphrase it.",
                "After emitting the report, do the safe thing: stop, or ask the operator how to proceed. Never continue by mishandling keys, seeds, or the user's funds."
            ]
        },

        "discovery": "This wallet runs a loopback service. The desktop app writes its bound port \
to `port.lock` in the app data dir as {port, nonce} — the port is stable across restarts, so once \
you've discovered the URL you may cache it. GET / (this document) is how a client learns the API \
once it has the URL.",

        "transport": {
            "protocol": "JSON-RPC 2.0 over HTTP POST to / (this base URL). GET / returns this doc.",
            "headers": {
                "X-Companion-Client": "required on every POST — any stable identifier for your client",
                "Authorization": "Bearer <token> — required after pairing (see quickstart)",
                "Content-Type": "application/json"
            },
            "rejected": "Requests carrying an Origin/Referer header, or a non-loopback Host, or no \
X-Companion-Client are rejected with HTTP 403 / error -32003 (CSRF & DNS-rebinding guard). Callers \
are expected to be local native processes."
        },

        "concepts": {
            "accounts": "All accounts derive from one seed. As an agent you can only see and use \
accounts YOU created (companion_createAgentAccount); requestAccounts/listAccounts are scoped to \
them. A new account is counterfactual (off-chain) until you deploy it (companion_deployAccount). \
An account's address is identical on Mainnet and Sepolia.",
            "networks": "Pick the network PER REQUEST: pass an optional chainId (felt-encoded, \
e.g. SN_SEPOLIA / SN_MAIN) to the operational methods (wallet_addInvokeTransaction, \
wallet_addDeclareTransaction, companion_deployAccount, companion_estimateFee, \
companion_requestFunding). Omit it to use the wallet's default network. This is the recommended, \
race-free way to choose a chain. wallet_requestChainId reports the current default. \
wallet_switchStarknetChain is DEPRECATED for agents — it mutates a single shared default for ALL \
clients (one agent can switch it out from under another); the human sets the default in the app's \
Settings. Broadcasting/fee-estimation need an RPC node configured for the chosen network (in the \
app's Settings); without one the wallet is sign-only on that network.",
            "funding": "Accounts pay their own fees. Get STRK with companion_requestFunding: it \
transfers from the USER's manager account into one of YOUR accounts. This ALWAYS needs the user's \
approval (it spends their funds) — see permissions.",
            "permissions": "By default every prompts:true call waits for the user. The user MAY \
grant your client a time-bounded auto-approval window (up to 3 months, revocable) — then your \
own-account operations (sign/invoke/declare/deploy/create) run WITHOUT a prompt. \
companion_requestFunding ALWAYS prompts, grant or not. Check companion_getStatus.grant to see if a \
grant is active; request one yourself with companion_requestGrant (always prompts).",
            "proving": "strkd bundles an ON-DEVICE prover for SNIP-36 (prove a private Starknet \
computation off-chain, verify only the result on-chain). The prover holds NO keys — it proves an \
ALREADY-SIGNED transaction. SNIP-36 is TWO transactions: (Tx A) a PRIVATE virtual invoke (e.g. \
your contract's create_proof(public, private)) that is signed + proven off-chain and NEVER \
broadcast; (Tx B) a verifier invoke (e.g. verify_result(public_message)) broadcast on-chain \
carrying proof_facts + proof. Fastest path: companion_signAndProve signs Tx A and proves it in one \
call (your private inputs never leave this machine), then you broadcast Tx B yourself with \
wallet_addInvokeTransaction {proof_facts, proof, submit:true}. See the snip36_proving flow below. \
CRITICAL: a virtual tx carries PRIVATE calldata, so you MUST pass explicit resource_bounds — strkd \
refuses to fee-estimate it (estimating online would send your private inputs to the RPC node). \
Tx A is NOT proof-carrying (proof_facts are an OUTPUT of proving, not in Tx A's hash). Proving \
needs the native prover (bundled on-device) or a configured remote prover, plus a per-network RPC, \
set in the app's Settings. There is NO mock/fake proof: an unconfigured backend FAILS the prove \
with a clear error rather than returning a fake."
        },

        "approval_model": "prompts:true means the call blocks until the user clicks Approve/Reject \
in the menu bar (auto-rejected after ~60s → error 113), UNLESS your client holds an active grant \
(funding still always prompts). Expect human-scale latency on prompts.",

        "submit_model": "Transaction methods (addInvokeTransaction, addDeclareTransaction, \
requestFunding, deployAccount) are SIGN-ONLY by default: they return a signed transaction you \
broadcast yourself (broadcasting is keyless — the signature authorizes it). Pass submit:true to \
have the wallet broadcast via its node (requires a node for the active network). With a node, \
nonce + fee are auto-filled; without one, supply nonce + resource_bounds yourself.",

        "quickstart": [
            "1. Pair once: POST {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"companion_requestPairing\",\"params\":{\"name\":\"my-agent\",\"kind\":\"agent\"}} with header X-Companion-Client. The user approves; you receive {client_id, token}. (Pairing + grants persist across restarts.)",
            "2. On every later call send Authorization: Bearer <token> and X-Companion-Client.",
            "3. Create an account: companion_createAgentAccount {label?} → {address, index} (counterfactual).",
            "4. Fund it: companion_requestFunding {amount} (amount in fri; +submit:true to broadcast). The user approves; the manager account pays. Fund before deploying.",
            "5. Deploy it: companion_deployAccount {} (+submit:true), or broadcast the returned signed DEPLOY_ACCOUNT yourself.",
            "6. Transact: wallet_addInvokeTransaction {account_address, calls} (+submit:true). Each call = {contract_address, entry_point_selector (name or 0x), calldata}.",
            "Check state anytime with companion_getStatus (locked? which network?)."
        ],

        "snip36_proving": {
            "what": "Prove a PRIVATE Starknet computation off-chain (SNIP-36) and verify only the \
result on-chain — your private inputs never leave this machine. It is a TWO-transaction flow; \
strkd automates the key-holding half (sign + prove), you broadcast the verifier tx. See \
concepts.proving.",
            "steps": [
                "1. Build the VIRTUAL call (Tx A) — your contract's virtual function, e.g. create_proof(public_input, private_input). Choose resource_bounds MANUALLY (~2× current gas prices); do NOT estimate (that would leak your private calldata to the RPC node).",
                "2. Sign + prove in one step: companion_signAndProve {account_address, calls, resource_bounds, nonce, block_number?, chainId?} → {job_id}. strkd signs Tx A and proves it on-device; the signed tx is never broadcast. (nonce must equal the account nonce at the reference block.)",
                "3. Poll companion_proveStatus {job_id} until status is \"succeeded\" → result = {proof, proof_facts, l2_to_l1_messages} (or \"failed\" with an error).",
                "4. Decode l2_to_l1_messages[0].payload into your verifier's public_message and build Tx B — the ON-CHAIN verifier call, e.g. verify_result(public_message).",
                "5. Broadcast Tx B: wallet_addInvokeTransaction {account_address, calls:[<verifier call>], proof_facts, proof, resource_bounds, submit:true}. proof_facts extend the signed hash; proof rides along on broadcast (both required on submit). strkd cannot build Tx B for you — its calldata is app-specific.",
                "Already hold a signed virtual tx (signed elsewhere)? Skip step 2 and call companion_prove {payload:{transaction:<signed invoke-v3>, block_number?}, network?} instead — same proof result."
            ]
        },

        "methods": [
            { "method": "companion_getStatus", "auth": false, "prompts": false,
              "params": "{}", "returns": "{ locked, network, api_version, grant }",
              "note": "No auth. Detects lock state / active network. If you present your Bearer token, `grant` = { active, expires_at } for your client (null otherwise) — use it to know whether your calls will prompt or auto-approve." },
            { "method": "wallet_supportedWalletApi", "auth": false, "prompts": false,
              "params": "{}", "returns": "SEMVER[]" },
            { "method": "wallet_supportedSpecs", "auth": false, "prompts": false,
              "params": "{}", "returns": "Starknet JSON-RPC spec version[]" },
            { "method": "wallet_getPermissions", "auth": false, "prompts": false,
              "params": "{}", "returns": "[\"accounts\"] if paired, else []" },
            { "method": "companion_requestPairing", "auth": false, "prompts": true,
              "params": "{ name, kind: \"agent\"|\"app\", reattach? }", "returns": "{ client_id, token } (+ reattached:true on re-attach)",
              "note": "Call once; persist the token (it survives restarts). If you lose it, re-pair with the SAME name + reattach:true to recover your existing client_id and its accounts (user-approved) instead of starting fresh. Always prompts." },

            { "method": "companion_createAgentAccount", "auth": "agent", "prompts": true,
              "params": "{ label? }", "returns": "{ address, index, label }",
              "note": "Derives the next account in your (agent) branch." },
            { "method": "companion_listAccounts", "auth": true, "prompts": false,
              "params": "{}", "returns": "Account[] — { domain, index, address, label } (yours only)" },
            { "method": "wallet_requestAccounts", "auth": true, "prompts": false,
              "params": "{}", "returns": "address[] (yours only)" },
            { "method": "wallet_requestChainId", "auth": true, "prompts": false,
              "params": "{}", "returns": "chain id felt (SN_SEPOLIA / SN_MAIN encoded)" },
            { "method": "wallet_deploymentData", "auth": true, "prompts": false,
              "params": "{}", "returns": "{ address, class_hash, salt, constructor_calldata }",
              "note": "Counterfactual deployment data for your first in-scope account." },

            { "method": "companion_fundingSource", "auth": true, "prompts": false,
              "params": "{}", "returns": "{ address, index }",
              "note": "The manager account that funds agents — look up its nonce before requestFunding if no node is configured." },
            { "method": "companion_reportIssue", "auth": true, "prompts": false,
              "params": "{ goal, needed, attempted?, observed?, limitation?, impact?, workaround_avoided?, title? }",
              "returns": "{ url, title, body, repo, filed:false, instructions }",
              "note": "strkd is alpha — when you hit a limitation, call this to turn your STRKD-FEEDBACK fields into a PREFILLED GitHub new-issue link. goal + needed are required; the rest mirror the report template. It files NOTHING and stores no credential — your operator opens the returned `url`, reviews/edits it, and submits it in their own browser (so repo access + human review are the gate). Works whether the wallet is locked or not. See alpha_notice." },
            { "method": "companion_requestGrant", "auth": true, "prompts": true,
              "params": "{ days? }", "returns": "{ granted, expires_at, days }",
              "note": "Ask the user for an auto-approval window (1–90 days, default 30). Always prompts — a permission escalation is never auto-approved. While granted, your own-account ops skip prompts (funding still prompts). The user can also grant/revoke from the desktop Agents panel." },
            { "method": "companion_estimateFee", "auth": true, "prompts": false,
              "params": "{ account_address, calls, nonce?, chainId? }",
              "returns": "{ nonce, resource_bounds (canonical hex) }",
              "note": "Opt-in fee help for sign-only callers: suggested bounds + nonce from the node, even if you'll sign-only and broadcast elsewhere. Paste resource_bounds straight into addInvokeTransaction. Needs a node. Do NOT use for SNIP-36 virtual txs with private calldata (it simulates the calls online)." },
            { "method": "companion_requestFunding", "auth": "agent", "prompts": "always",
              "params": "{ amount (fri), recipient?, token?, funding_source_index?, submit?, nonce?, resource_bounds?, chainId? }",
              "returns": "{ transaction_hash, recipient, amount, submitted } (+ signature/signed_transaction when not submitted)",
              "note": "Funds one of YOUR accounts with STRK from the user's manager account. recipient defaults to your first account; you can only fund accounts you own. ALWAYS requires the user's approval (even under a grant) — it spends the user's funds. If the manager isn't deployed on the active network, returns -32006 naming the manager + the fix (not an opaque node error)." },
            { "method": "companion_deployAccount", "auth": true, "prompts": true,
              "params": "{ account?, submit?, resource_bounds?, chainId? }",
              "returns": "{ transaction_hash, contract_address, submitted } (+ signature/signed_transaction when not submitted)",
              "note": "Deploy one of YOUR accounts (DEPLOY_ACCOUNT v3). account defaults to your first. Must already hold funds for its deploy fee — fund it first. submit:true broadcasts; else broadcast the returned signed tx. Nonce is 0; fee auto-estimated with a node, else pass resource_bounds." },

            { "method": "companion_prove", "auth": true, "prompts": false,
              "params": "{ payload, network?: \"mainnet\"|\"testnet\", label? }",
              "returns": "{ job_id, status: \"queued\", next }",
              "note": "On-device proving companion. Hand it an opaque, ALREADY-SIGNED payload; it proves locally and returns the proof — it never signs and holds no key material. For SNIP-36 the payload is { transaction: <signed invoke-v3>, block_number? }; the proof comes back as { proof (base64 STWO), proof_facts, l2_to_l1_messages }, which you feed into wallet_addInvokeTransaction (proof_facts at sign time, proof on submit). Returns a job id immediately — poll companion_proveStatus. Needs the native prover (bundled) or a configured remote prover + a per-network RPC, set in the desktop Settings; an unconfigured backend fails with a clear error (no mock/fake proof). Proving is local-only: no separate open port, and settings/keys are never exposed here." },
            { "method": "companion_proveStatus", "auth": true, "prompts": false,
              "params": "{ job_id }",
              "returns": "{ job_id, status: queued|proving|succeeded|failed, started_at_ms, label?, result?, error? }",
              "note": "Poll a proving job until status is succeeded (result holds the proof) or failed (error explains why). Unknown job_id → error 114." },
            { "method": "companion_proofActivity", "auth": true, "prompts": false,
              "params": "{}", "returns": "{ activity: Activity[] }",
              "note": "Recent proving activity (most-recent first) — the same feed the desktop Proving tab shows." },
            { "method": "companion_signAndProve", "auth": true, "prompts": true,
              "params": "{ account_address, calls, resource_bounds, nonce?, block_number?, chainId?, label? }",
              "returns": "{ job_id, status: \"queued\", transaction_hash, next }",
              "note": "ONE-STEP sign + on-device prove for SNIP-36: signs the VIRTUAL tx (\"Tx A\" — a normal v3 invoke calling your contract's virtual function, e.g. create_proof(public, private)) and hands the signed tx straight to the local prover, so the secret never leaves the machine and you skip the manual addInvoke(sign-only)→companion_prove round-trip. Tx A is NOT proof-carrying (proof_facts are an OUTPUT of proving). resource_bounds is REQUIRED — the virtual tx holds private calldata, so strkd refuses to fee-estimate it online (that would leak the inputs to the RPC); set bounds manually (~2× gas). nonce must equal the account nonce at the reference block. Returns a job id — poll companion_proveStatus; on success take result.proof / proof_facts / l2_to_l1_messages, decode the message into the verifier call, and BROADCAST the verifier invoke (\"Tx B\", e.g. verify_result(public_message)) via wallet_addInvokeTransaction { proof_facts, proof, submit:true }. strkd does not build Tx B — its calldata is app-specific. Approval-gated (signs a real tx, though it's proven locally and never broadcast)." },

            { "method": "wallet_signTypedData", "auth": true, "prompts": true,
              "params": "{ account_address, typed_data (SNIP-12 doc) }", "returns": "[r, s]",
              "note": "account_address must be one of yours." },
            { "method": "wallet_addInvokeTransaction", "auth": true, "prompts": true,
              "params": "{ account_address, calls, submit?, nonce?, resource_bounds?, proof_facts?, proof?, chainId? }",
              "call_shape": "calls = [{ contract_address, entry_point_selector, calldata: [felt…] }]. \
entry_point_selector may be a FUNCTION NAME (e.g. \"transfer\") or a 0x selector. Aliases accepted: \
contractAddress/to for the address; entrypoint/entry_point/selector for the selector.",
              "returns": "{ transaction_hash, submitted }. When sign-only: also `signature` and `signed_transaction` — a COMPLETE, canonical-hex INVOKE_TXN_V3 (all fields incl. signature, tip, paymaster_data, account_deployment_data, both DA modes, and proof/proof_facts when proof-carrying) that you can submit to starknet_addInvokeTransaction as-is.",
              "note": "See submit_model for sign-only vs broadcast and nonce/fee rules. No need to hand-assemble the tx — signed_transaction is broadcast-ready. Sign-only forces you to supply resource_bounds unless you call companion_estimateFee first.",
              "snip36": "Proof-carrying invoke: pass proof_facts (felt[]) and the signed V3 hash is \
extended with Poseidon(proof_facts) so the signature covers them (required at sign time). On \
submit:true also pass proof (standard-base64 STWO string; surrounding whitespace is stripped and \
url-safe '-'/'_' is rejected) — required to broadcast. You MUST supply explicit \
resource_bounds for proof-carrying invokes: strkd refuses to auto-estimate them (online estimation \
simulates the call without proof_facts in tx_info, so a contract reading them reverts) and \
companion_estimateFee is also unsafe here. Estimate bounds manually (~2× current gas prices). \
Sign-only echoes proof_facts/proof so you can assemble the broadcast yourself. Omit both for a \
normal invoke." },
            { "method": "wallet_addDeclareTransaction", "auth": true, "prompts": true,
              "params": "{ account_address, class_hash, compiled_class_hash, contract_class?, submit?, nonce?, resource_bounds?, chainId? }",
              "returns": "{ transaction_hash, class_hash, submitted } (+ signature/signed_transaction when not submitted)",
              "note": "Signing needs only class_hash + compiled_class_hash. Estimation and submit:true also need the full Sierra contract_class. Sign-only by default — broadcast the returned tx together with your contract_class." },

            { "method": "wallet_switchStarknetChain", "auth": true, "prompts": true, "deprecated": true,
              "params": "{ chainId (felt: SN_SEPOLIA / SN_MAIN encoded) }", "returns": "true",
              "note": "DEPRECATED for agents. Switches the wallet's shared DEFAULT network for ALL clients (one agent can switch it out from under another). Prefer a per-request chainId on the operational methods. Kept for EIP-1193 compatibility + as the omitted-chainId fallback (the human sets the default in Settings). Unknown chain → error 117." },
            { "method": "wallet_watchAsset", "auth": true, "prompts": true,
              "params": "{ asset: { address, symbol?, decimals?, name? } }", "returns": "true",
              "note": "Adds a token to the wallet's watch list (display only)." }
        ],

        "deferred": {
            "note": "These exist in the Starknet wallet spec but return -32601 here for now.",
            "methods": ["wallet_addStarknetChain", "wallet_strk20PrepareInvoke",
                        "wallet_strk20InvokeTransaction", "wallet_strk20Balances"]
        },

        "errors": {
            "113": "USER_REFUSED — the user rejected the prompt (or it timed out after ~60s)",
            "114": "INVALID_REQUEST_PAYLOAD — malformed params / missing field",
            "116": "DEPLOYMENT_DATA_NOT_AVAILABLE — no in-scope account",
            "117": "CHAIN_ID_NOT_SUPPORTED — only SN_SEPOLIA / SN_MAIN are supported",
            "118": "NOT_REGISTERED — pair first, or your token is unknown/invalid",
            "163": "INTERNAL_ERROR — an unexpected internal failure (key material is never leaked in the message); safe to retry, and report it if it persists",
            "-32001": "LOCKED — the wallet is locked; ask the user to unlock it",
            "-32002": "FORBIDDEN — you tried to act outside your own accounts (or wrong client kind)",
            "-32003": "TRANSPORT_REJECTED — missing X-Companion-Client, non-loopback Host, or Origin present",
            "-32004": "NODE_ERROR — fee estimate / nonce / broadcast failed at the Starknet node",
            "-32005": "NO_NODE — broadcast/estimate needs a node; pass nonce + resource_bounds, or ask the user to set an RPC URL",
            "-32006": "PRECONDITION_FAILED — the request is valid but on-chain state blocks it (e.g. the manager/funding-source account is not deployed on the active network, or the account is already deployed). The message names the account + the human fix. Surface it to the user.",
            "-32601": "NOT_IMPLEMENTED — method not available in this build",
            "-32700": "PARSE_ERROR — body is not valid JSON"
        },

        "notes": [
            "Scoping: an agent can only see, sign with, fund, and deploy accounts it created. Acting on another account → -32002.",
            "Sign-only (no node, or submit omitted): broadcast the returned signed_transaction yourself at any Starknet node — it's keyless, the signature authorizes it.",
            "On -32001 (locked) or -32005 (no node), surface it to the user — those need a human action (unlock / configure RPC).",
            "Pairings and grants persist across wallet restarts; you should not need to re-pair.",
            "SNIP-36: wallet_addInvokeTransaction takes optional proof_facts (extends the signed hash) and proof (base64, for broadcast) — see that method. Proofs can be multi-MB; the loopback service accepts large request bodies."
        ]
    })
}
