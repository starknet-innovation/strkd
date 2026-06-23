//! `strkd` — command-line companion for the strkd menu-bar wallet.
//!
//! A thin client over the loopback JSON-RPC service. It holds no keys: signing
//! and approvals stay in the desktop app, which pops a confirmation for any
//! state-changing request. Covers discovery, pairing, read-only status/account
//! listing, off-chain signing, and invoke transactions.
//!
//! Output convention: the requested result is JSON on **stdout**; human
//! progress notes go to **stderr**, so `strkd accounts | jq` works as-is. See
//! [`Output`] for how `--json` / `--quiet` adjust this.

mod client;
mod discovery;
mod token;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(
    name = "strkd",
    version,
    about = "CLI companion for the strkd menu-bar wallet",
    long_about = "Talks to the running strkd menu-bar app over its local service.\n\
                  Start the app first, run `strkd pair` once, then use the other commands."
)]
struct Cli {
    /// Machine-readable mode: only JSON on stdout, no human progress text.
    /// Errors are emitted as `{\"error\": ...}` on stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Suppress progress/notice messages (the requested output still prints).
    #[arg(long, short, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pair this CLI with the wallet (approve the prompt in the menu bar).
    Pair {
        /// Display name shown in the approval prompt and request log.
        #[arg(long, default_value = "strkd-cli")]
        name: String,
        /// Client kind: `app` (interactive) or `agent` (scoped accounts).
        #[arg(long, default_value = "app", value_parser = ["app", "agent"])]
        kind: String,
    },
    /// Show wallet status: lock state, network, and this client's grant.
    Status,
    /// Print the wallet's self-describing usage document.
    Usage,
    /// List the accounts this CLI is allowed to see and use.
    Accounts,
    /// Sign SNIP-12 typed data with an account (off-chain signature).
    Sign {
        /// Address of the signing account.
        #[arg(long)]
        account: String,
        /// Typed data: inline JSON, or `@path` to read it from a file.
        #[arg(long)]
        data: String,
    },
    /// Build and sign an invoke transaction; with `--submit`, broadcast it.
    Send {
        /// Address of the sending account.
        #[arg(long)]
        account: String,
        /// Target contract address (single-call form).
        #[arg(long, required_unless_present = "calls")]
        to: Option<String>,
        /// Function name or `0x` selector (single-call form).
        #[arg(long, required_unless_present = "calls")]
        function: Option<String>,
        /// Comma-separated `0x` calldata felts (single-call form).
        #[arg(long, requires = "to")]
        calldata: Option<String>,
        /// Full `calls` array as inline JSON or `@path` (multi-call form).
        #[arg(long, conflicts_with_all = ["to", "function", "calldata"])]
        calls: Option<String>,
        /// Broadcast the transaction (IRREVERSIBLE). Default: sign only.
        #[arg(long)]
        submit: bool,
        /// Network override: `sepolia` or `mainnet`. Default: the wallet's
        /// active network.
        #[arg(long)]
        network: Option<String>,
    },
}

/// How results and notices are rendered, per the global `--json` / `--quiet`
/// flags. Keeps the stdout/stderr split in one place.
struct Output {
    json: bool,
    quiet: bool,
}

impl Output {
    /// A human progress/notice line → stderr. Suppressed in `--json` (machine
    /// contract) and `--quiet` (explicitly silenced) modes.
    fn note(&self, msg: &str) {
        if !self.json && !self.quiet {
            eprintln!("{msg}");
        }
    }

    /// The command's result value → stdout, as pretty JSON (pipeable to `jq`).
    fn emit(&self, v: &Value) {
        println!(
            "{}",
            serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
        );
    }

    /// Report a fatal error → stderr. JSON object in `--json` mode, else a
    /// plain `error: ...` line.
    fn fail(&self, msg: &str) {
        if self.json {
            eprintln!("{}", json!({ "error": msg }));
        } else {
            eprintln!("error: {msg}");
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let out = Output {
        json: cli.json,
        quiet: cli.quiet,
    };
    if let Err(e) = run(cli.command, &out).await {
        out.fail(&e);
        std::process::exit(1);
    }
}

async fn run(command: Command, out: &Output) -> Result<(), String> {
    let client = client::Client::connect()?;
    match command {
        Command::Pair { name, kind } => {
            out.note("Requesting pairing — approve the prompt in the strkd menu bar…");
            let result = client
                .call(
                    "companion_requestPairing",
                    json!({ "name": name, "kind": kind }),
                    None,
                )
                .await?;
            let pairing: token::Pairing = serde_json::from_value(result)
                .map_err(|e| format!("unexpected pairing response: {e}"))?;
            token::save(&pairing)?;
            // Never surface the bearer token on stdout — it lives only in the
            // 0600 token file. Confirm with the non-secret client_id.
            if out.json {
                out.emit(&json!({ "client_id": pairing.client_id, "paired": true }));
            } else if !out.quiet {
                println!("Paired. client_id = {}", pairing.client_id);
            }
        }
        Command::Status => {
            // Sending our token (if any) makes the wallet include grant state.
            let saved = token::load();
            let result = client
                .call(
                    "companion_getStatus",
                    json!({}),
                    saved.as_ref().map(|p| p.token.as_str()),
                )
                .await?;
            out.emit(&result);
        }
        Command::Usage => out.emit(&client.usage().await?),
        Command::Accounts => {
            let token = require_token()?;
            let result = client
                .call("companion_listAccounts", json!({}), Some(&token))
                .await?;
            out.emit(&result);
        }
        Command::Sign { account, data } => {
            let token = require_token()?;
            let typed_data = read_json_arg(&data)?;
            out.note("Signing typed data — approve the prompt in the strkd menu bar…");
            let result = client
                .call(
                    "wallet_signTypedData",
                    json!({ "account_address": account, "typed_data": typed_data }),
                    Some(&token),
                )
                .await?;
            out.emit(&result);
        }
        Command::Send {
            account,
            to,
            function,
            calldata,
            calls,
            submit,
            network,
        } => {
            let token = require_token()?;
            let calls_val = match calls {
                Some(raw) => read_json_arg(&raw)?,
                // Single-call form. clap guarantees `to`+`function` are present
                // here (required_unless_present = "calls").
                None => {
                    let mut call = json!({
                        "contract_address": to.expect("clap enforces --to"),
                        "entry_point_selector": function.expect("clap enforces --function"),
                    });
                    if let Some(cd) = calldata {
                        call["calldata"] = Value::Array(parse_calldata(&cd));
                    }
                    json!([call])
                }
            };
            let mut params = json!({
                "account_address": account,
                "calls": calls_val,
                "submit": submit,
            });
            if let Some(net) = network {
                params["chainId"] = Value::String(chain_id_felt(&net)?);
            }
            out.note(&format!(
                "{} transaction — approve the prompt in the strkd menu bar…",
                if submit { "Submitting" } else { "Signing" }
            ));
            let result = client
                .call("wallet_addInvokeTransaction", params, Some(&token))
                .await?;
            out.emit(&result);
        }
    }
    Ok(())
}

/// Load the saved pairing token, or fail with a pointer to `strkd pair`.
fn require_token() -> Result<String, String> {
    token::load()
        .map(|p| p.token)
        .ok_or_else(|| "not paired yet — run `strkd pair` first".to_string())
}

/// Read a JSON argument that is either inline JSON or `@path-to-file`.
fn read_json_arg(s: &str) -> Result<Value, String> {
    let raw = match s.strip_prefix('@') {
        Some(path) => {
            std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?
        }
        None => s.to_string(),
    };
    serde_json::from_str(&raw).map_err(|e| format!("invalid JSON: {e}"))
}

/// Split a `0x..,0x..` calldata string into JSON felt strings. The service
/// validates each is a real felt.
fn parse_calldata(s: &str) -> Vec<Value> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| Value::String(x.to_string()))
        .collect()
}

/// Map a friendly network name to its felt-encoded chain id (the form the
/// service's `chainId` param expects). These are stable Starknet protocol
/// constants — the short-string encodings of `SN_SEPOLIA` / `SN_MAIN`.
fn chain_id_felt(network: &str) -> Result<String, String> {
    match network.to_ascii_lowercase().as_str() {
        "sepolia" | "sn_sepolia" => Ok("0x534e5f5345504f4c4941".to_string()),
        "mainnet" | "main" | "sn_main" => Ok("0x534e5f4d41494e".to_string()),
        other => Err(format!("unknown network '{other}' (use sepolia or mainnet)")),
    }
}
