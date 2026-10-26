//! Ported from tests/test_routing_pin.py.
use crate::client::routing::Router;
use crate::config::{Config, ConfigHandle};

fn router(pinned: bool) -> Router {
    let mut cfg = Config::default();
    for (role, chain) in [("strong", "claude-sonnet-5-5"), ("debug", "claude-sonnet-5-5"), ("fast", "flash")] {
        cfg.routing.insert(role.into(), vec![chain.into()]);
    }
    cfg.model.name = "codex/gpt-5.6-luna".into();
    cfg.model_pinned = pinned;
    Router::new(ConfigHandle::new(cfg))
}

#[test]
fn escalation_and_the_debugger_stay_on_the_model_the_user_picked() {
    let r = router(true);
    assert_eq!(r.chain("strong"), ["codex/gpt-5.6-luna"]);
    assert_eq!(r.chain("debug"), ["codex/gpt-5.6-luna"]);
    assert_eq!(r.chain("fast")[0], "flash"); // cheap routing for small roles is a cost choice, not a swap of the main model
}

#[test]
fn without_a_pinned_model_the_configured_strong_chain_still_applies() {
    assert_eq!(router(false).chain("strong")[0], "claude-sonnet-5-5");
}
