//! The plan bridge tools (adoption 19): `plan_enter` (Build→Plan) and
//! `plan_exit` (Plan→Build). Both ask the operator via the shipped question
//! tool and, on approval, enqueue the next turn in the target mode.

pub struct PlanEnter;
impl PlanEnter {
    pub const NAME: &'static str = "plan_enter";
}

pub struct PlanExit;
impl PlanExit {
    pub const NAME: &'static str = "plan_exit";
}
