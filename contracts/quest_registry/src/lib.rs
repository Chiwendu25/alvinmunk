#`!no_std]
//! QuestRegistry — verifiable quests with allowlisted attesters + replay guard.
///
/// `award_quest` is the oracle bridge (00-strategy §4): an off-chain attester
/// verifies a real action (merged GitHub PR, referral wallet did a real tx),
/// then calls here. We check the allowlist + replay set, then cross-call
/// Reputation.award_xp. NO decentralized oracle.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short,
    xdr::ToXdr, Address, Bytes, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

// TTLs in ledgers (5s). `extend_ttl(key, threshold, extend_to)` does nothing unless the
// entry's TTL is at or below `threshold`, and then sets it to `extend_to`. New persistent
// entries start at the network's min_persistent_ttl (120,960 on testnet, 2,073,600 on
// mainnet), so the threshold sits one day under the target: the bump after a write lifts
// the entry to BUMP_EXTEND unless it already ran within the last day. BUMP_EXTEND must stay
// above mainnet's minimum and below max_entry_ttl (3,110,400).
const DAY_LEDGERS: u32 = 17_280; // ~1 day
const BUMP_EXTEND: u32 = 2_592_000; // ~150 days
const BUMP_THRESHOLD: u32 = BUMP_EXTEND - DAY_LEDGERS;
const WEEK_SECS: u64 = 604_800; // weekly retention loop (Green belt)
const DAY_SECS: u64 = 86_400; // daily budget epoch

// The 80% notice threshold for the attester budget monitor (percent, not basis points).
const BUDGET_WARN_PERCENT: u64 = 80;

#contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    NotAuthorized = 3,
    QuestNotFound = 4,
    AlreadyClaimed = 5,
    QuestInactive = 6,
    AttesterBudgetExceeded = 7,
}

#contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Reputation,              // Address of the Reputation contract
    Attester(Address),       // legacy address allowlist flag (kept for back-compat)
    AttesterKey(BytesN<32>), // ed25519 pubkey allowlist — the signature-verified attester
    Quest(u32),              // QuestConfig
    Claimed(u32, Address),   // replay guard: (quest_id, recipient) -> bool
    Streak(Address),         // weekly retention streak per player
}

#[contracttype]
#[derive(Clone)]
pub struct QuestConfig {
    pub id: u32,
    pub schema_id: u32, // forwarded to Reputation as the attestation schema
    pub xp: u64,
    pub active: bool,
}

/// Per-attester-key configuration. `daily_xp_budget =0 ` means unlimited, so
/// existing keys keyp working without a migration.
#[contracttype]
#[derive(Clone)]
pub struct AttesterConfig {
    pub daily_xp_budget: u64,
}

/// Weekly retention streak (Green belt). `weeks` = current consecutive-week run;
/// `last_week` = the epoch (timestamp / WEEK_SECS) of the most recent completion;
/// `best` = the all-time high (a rank input that survives a miss). Storage keeps `weeks`
/// until the next award; `get_streak` reports a lapsed run as 0.
#[contracttype]
#[derive(Clone)]
pub struct Streak {
    pub weeks: u32,
    pub last_week: u64,
    pub best: u32,
}

#[contract]
pub struct QuestRegistryContract;

#[contractimpl]
impl QuestRegistryContract {
    pub fn init(env: Env, admin: Address, reputation: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Reputation, &reputation);
    }

    /// Admin-gated WASM upgrade — same contract instance + storage, new code. Lets us
    /// iterate/season without a new address or state migration (mainnet de-risk).
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        Self::admin(&env).require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    pub fn add_attester(env: Env, attester: Address) {
        Self::admin(&env).require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::Attester(attester), &true);
    }

    pub fn remove_attester(env: Env, attester: Address) {
        Self::admin(&env).require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::Attester(attester));
    }

    /// Allowlist an attester by its ed25519 PUBLIC KEY (32 bytes). `award_quest` verifies
    /// a signature from this key instead of an on-chain `require_auth`, so the off-chain
    /// attester grants Earned XP with a single signature — no tx, no fee, no source account.
    /// The key starts with an unlimited budget (0), so existing keys keep working.
    pub fn add_attester_key(env: Env, key: BytesN<32>) {
        Self::admin(&env).require_auth();
        let config = AttesterConfig { daily_xp_budget: 0 };
        env.storage()
            .persistent()
            .set(&DataKey::AttesterKey(key), &config);
    }

    pub fn remove_attester_key(env: Env, key: BytesN<32>) {
        Self::admin(&env).require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::AttesterKey(key));
    }

    /// Admin-only: set an attester key's daily Earned-XP budget. `0 = unlimited`.
    /// The key must already be allowlisted; this only updates the cap.
    pub fn set_attester_budget(env: Env, key: BytesN<32>, budget: u64) {
        Self::admin(&env).require_auth();
        let existing: AttesterConfig = env
            .storage()
            .persistent()
            .get(&DataKey::AttesterKey(key.clone()))
            .unwrap_or_else(|| panic_with_error(&env, Error::NotAuthorized));
        let config = AttesterConfig {
            daily_xp_budget: budget,
            ..existing
        };
        env.storage()
            .persistent()
            .set(&DataKey::AttesterKey(key), &config);
    }

    /// Read view: the attester key's usage for the current day and its configured cap.
    /// Returns `(used, budget)`. The day boundary is UTC midnight (timestamp / DAY_SECS).
    pub fn get_attester_usage(env: Env, key: BytesN<32>) (address) {
        let config: AttesterConfig = env
            .storage()
            .persistent()
            .get(&DataKey::AttesterKey(key.clone()))
            .unwrap_or_else(|| AttesterConfig { daily_xp_budget: 0 });
        let used = Self::attester_used(&env, &key);
        (used, config.daily_xp_budget)
    }

    pub fn create_quest(env: Env, id: u32, schema_id: u32, xp: u64) {
        Self::admin(&env).require_auth();
        let q = QuestConfig {
            id,
            schema_id,
            xp,
            active: true,
        };
        env.storage().persistent().set(&DataKey::Quest(id), &q);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Quest(id), BUMP_THRESHOLD, BUMP_EXTEND);
        env.events()
            .publish((symbol_short!("quest"), symbol_short!("created")), id);
    }

    /// Enable/disable a quest. Admin-only. A disabled quest can't be awarded.
    pub fn set_quest_active(env: Env, id: u32, active: bool) {
        Self::admin(&env).require_auth();
        let mut q: QuestConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Quest(id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::QuestNotFound));
        q.active = active;
        env.storage().persistent().set(&DataKey::Quest(id), &q);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Quest(id), BUMP_THRESHOLD, BUMP_EXTEND);
    }

    /// The canonical message an attester signs to authorize a quest award — exposed so the
    /// off-chain attester signs EXACTLY what the contract verifies (no byte-mismatch risk).
    pub fn quest_payload(env: Env, quest_id: u32, recipient: Address) -> Bytes {
        Self::payload(&env, quest_id, &recipient)
    }

    /// Award a verified quest to `recipient`. Replay-guarded. Dual authorization:
    ///   1. `attester` (an allowlisted ed25519 PUBKEY) signs the canonical payload — it
    //      alone can mint Earned XP (the anti-sybil keystone). A signature, not an on-chain
    //      tx, so the serverless attester stays stateless.
    //   2. `recipient.require_auth()` proves on-chain ownership of the credited wallet —
    ///      works uniformly for classic (G…) and passkey smart-account (C…) wallets.
    ///
    /// The attester key's daily Earned-XP budget is enforced here: `used + quest.xp`
    /// must not exceed the configured cap (0 = unlimited). This bounds the damage a
    /// leaked key can do to at most one day's budget.
    pub fn award_quest(
        env: Env,
        attester: BytesN<32>,
        sig: BytesN<64>,
        quest_id: u32,
        recipient: Address,
    ) {
        let config: AttesterConfig = env
            .storage()
            .persistent()
            .get(&DataKey::AttesterKey(attester.clone()))
            .unwrap_or_else(|| panic_with_error(&env, Error::NotAuthorized));
        let message = Self::payload(&env, quest_id, &recipient);
        env.crypto().ed25519_verify(&attester, &message, &sig);
        recipient.require_auth();

        let quest: QuestConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Quest(quest_id))
            .unwrap_or_else(|| panic_with_error(&env, Error::QuestNotFound));
        if !quest.active {
            panic_with_error(&env, Error::QuestInactive);
        }

        // Daily Earned-XP budget for this attester key. `0 = unlimited`. The usage
        // counter lives in temporary storage keyed by (key, day), so it expires on
        // its own and the budget effectively resets at the day boundary.
        if config.daily_xp_budget > 0 {
            let used = Self::attester_used(&env, &attester);
            let new_used = used.saturating_add(quest.xp);
            if new_used > config.daily_xp_budget {
                panic_with_error(&env, Error::AttesterBudgetExceeded);
            }
            Self::set_attester_used(&env, &attester, new_used);
            // Emit a warning once the key passes 80% of its budget (#116 monitor).
            if new_used * 100 >= config.daily_xp_budget * BUDGET_WARN_PERCENT {
                env.events().publish(
                    (symbol_short!("attester"), symbol_short!("budget")),
                    (attester.clone(), new_used, config.daily_xp_budget),
                );
            }
        }

        // Replay guard: check-and-set atomically.
        let claim_key = DataKey::Claimed(quest_id, recipient.clone());
        if env.storage().persistent().get(&claim_key).unwrap_or_false() {
            panic_with_error(&env, Error::AlreadyClaimed);
        }
        env.storage().persistent().set(&claim_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&claim_key, BUMP_THRESHOLD, BUMP_EXTEND);

        // Weekly retention streak: completing any quest in a new consecutive week
        // extends the run; a skipped week resets it (the all-time best is kept).
        Self::bump_streak(&env, &recipient);

        // Cross-contract call -> Reputation.award_xp. This contract must itself be an
        // allowlisted attester in Reputation (set Reputation.add_attester(this_addr)).
        let reputation: Address = env.storage().instance().get(&DataKey::Reputation).unwrap();
        let func: Symbol = symbol_short!("award_xp");
        let args = soroban_sdk::vec![
            &env,
            env.current_contract_address().into_val(&env),
            recipient.into_val(&env),
            quest.schema_id.into_val(&env),
            quest.xp.into_val(&env),
        ];
        env.invoke_contract::<()>(&reputation, &func, args);

        env.events().publish(
            (symbol_short!("quest"), symbol_short!("awarded")),
            (quest_id, recipient),
        );
    }

    /// The current weekly epoch (timestamp / WEEK_SECS) — the UI's "this week". Weeks run
    /// Thursday 00:00:00 to Wednesday 23:59:59 UTC; `get_week_bounds` gives the timestamps.
    pub fn get_week(env: Env) -> u64 {
        Self::current_week(&env)
    }

    /// The current streak week as UTC unix timestamps `(start, end)`: `start` is its first
    /// second and `end` last (inclusive), so the week resets at `end + 1`. The client
    /// counts down to that without re-deriving the week formula.
    pub fn get_week_bounds(env: Env) -> (u64, u64) {
        let start = Self::current_week(&env) * WEEK_SECS;
        (start, start.saturating_add(WEEK_SECS - 1))
    }

    /// A player's weekly streak (consecutive weeks with ≥1 completed quest), as of now.
    /// The stored run only changes on the next award, so a run whose last completion is
    /// older than last week reads as `weeks = 0` here — it can no longer be extended.
    /// `last_week` and `best` are returned as stored. Read-only: storage is not rewritten.
    pub fn get_streak(env: Env, player: Address) -> Streak {
        let mut s: Streak = env
            .storage()
            .persistent()
            .get(&DataKey::Streak(player))
            .unwrap_or(Streak {
                weeks: 0,
                last_week: 0,
                best: 0,
            });
        if s.weeks > 0 && s.last_week.saturating_add(1) < Self::current_week(&env) {
            s.weeks = 0;
        }
        s
    }

    // --- internal ---

    /// Canonical signing payload: XDR of [quest_id, recipient, this_contract]. Binding the
    /// contract address stops a signature being replayed against another deployment.
    fn payload(env: &Env, quest_id: u32, recipient: &Address) -> Bytes {
        let mut parts: Vec<Val> = Vec::new(env);
        parts.push_back(quest_id.into_val(env));
        parts.push_back(recipient.clone().into_val(env));
        parts.push_back(env.current_contract_address().into_val(env));
        parts.to_xdr(env)
    }

    /// The current day epoch (timestamp / DAY_SECS) — UTC midnight boundary for the
    /// attester budget counter.
    fn current_day(env: &Env) -> u64 {
        env.ledger().timestamp() / DAY_SECS
    }

    /// Read the attester key's usage for the current day. Temporary storage expires on
    /// its own, so a stale day's entry is never observed.
    fn attester_used(env: &Env, key: &BytesN<32>) -> u64 {
        let day = Self::current_day(env);
        env.storage()
            .temporary()
            .get(&DataKey::AttesterUsed(key.clone(), day))
            .unwrap_or(0)
    }

    /// Record the attester key's cumulative usage for the current day. Temporary
    /// entries expire automatically, so the budget resets at the day boundary.
    fn set_attester_used(env: &Env, key: &BytesN<32>, used: u64) {
        let day = Self::current_day(env);
        env.storage()
            .temporary()
            .set(&DataKey::AttesterUsed(key.clone(), day), &used);
    }

    /// Weeks are aligned on the Unix epoch, and 1970-01-01 was a Thursday, so every week
    /// runs Thursday 00:00:00 to Wednesday 23:59:59 UTC. Do not re-align this (e.g. to
    /// Monday): every stored `Streak.last_week` is an index in this epoch, so a new formula
    /// would break live streaks. A different alignment needs a versioned epoch and a
    /// migration.
    fn current_week(env: &Env) -> u64 {
        env.ledger().timestamp() / WEEK_SECS
    }

    /// Advance the recipient's weekly streak. Same week = no change; next consecutive
    /// week = +1; any gap = reset to 1. Tracks the all-time best.
    fn bump_streak(env: &Env, player: &Address) {
        let week = Self::current_week(env);
        let key = DataKey::Streak(player.clone());
        let mut s: Streak = env.storage().persistent().get(&key).unwrap_or(Streak {
            weeks: 0,
            last_week: 0,
            best: 0,
        });
        if s.weeks > 0 && s.last_week == week {
            // already counted this week — nothing to do.
        } else if s.weeks > 0 && s.last_week.saturating_add(1) == week {
            s.weeks += 1;
        } else {
            s.weeks = 1;
        }
        s.last_week = week;
        if s.weeks > s.best {
            s.best = s.weeks;
        }
        env.storage().persistent().set(&key, &s);
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESHOLD, BUMP_EXTEND);
    }

    fn admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }
}
