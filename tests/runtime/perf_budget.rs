#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PerfBudget {
    open_terminals: usize,
    visible_terminals: usize,
    streaming_terminals: usize,
}

impl Default for PerfBudget {
    fn default() -> Self {
        Self {
            open_terminals: 20,
            visible_terminals: 6,
            streaming_terminals: 3,
        }
    }
}

impl PerfBudget {
    fn supports_baseline_target(&self) -> bool {
        self.open_terminals >= 20 && self.visible_terminals >= 6 && self.streaming_terminals >= 3
    }
}

#[test]
fn perf_budget_baseline_target_is_defined() {
    let budget = PerfBudget::default();

    assert_eq!(budget.open_terminals, 20);
    assert_eq!(budget.visible_terminals, 6);
    assert_eq!(budget.streaming_terminals, 3);
    assert!(budget.supports_baseline_target());
}
