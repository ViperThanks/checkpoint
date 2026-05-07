//! `agent-aspect rules` — 列出当前内置默认规则表。
//!
//! 以 Guard 模式实例化 RuleEngine 并打印所有规则，
//! 让用户直观看到每条规则的 ID、动作、来源、最低激活模式和描述。
//! D999（paranoid 兜底）和 D020（observer 模式说明）会额外附注。

use agent_aspect_core::rule::{Mode, RuleEngine};

/// 打印当前规则引擎中的全部默认规则。
///
/// 固定使用 Guard 模式初始化，因为规则表本身是全局共享的，
/// mode 只影响运行时评估逻辑，不影响规则列表。
pub fn cmd_rules() {
    let engine = RuleEngine::with_defaults(Mode::Guard);
    println!("mode: {}", engine.mode());
    println!();
    println!(
        "{:<6} {:<8} {:<12} {:<10} {}",
        "ID", "ACTION", "SOURCE", "MIN_MODE", "DESCRIPTION"
    );
    println!("{}", "-".repeat(75));
    for r in engine.rules() {
        println!(
            "{:<6} {:<8} {:<12} {:<10} {}",
            r.id, r.default_action, r.source, r.min_mode, r.description,
        );
    }
    println!();
    println!("D999: paranoid mode catch-all — unclassified operations → ask");
    println!("D020: observer mode — all rules evaluate but never intercept");
}
