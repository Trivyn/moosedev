//! Harness profiles. `MOOSEDEV_HARNESS_PROFILE=lean` keeps the harness's
//! grounding (knowledge, rules, the objective gather, the plan view, whole
//! source, language-server findings and the focus block as information), its
//! identity (plan approval, the final required checks, capture) and its
//! safety (sandbox and permissions, the action contract, the step cap), and
//! switches off the controls on the model's own actions: refusals, cut
//! copies, loop guards, finish gates and the harness taking the turn. The
//! model reads, inspects, searches and reruns freely, as a plain coding agent
//! does. Read per call, like every other lever. Requirement e9166711: control
//! can cost capability, so its balance is measured per model.

/// Whether the lean profile is on.
pub(in crate::harness::runner) fn lean() -> bool {
    std::env::var("MOOSEDEV_HARNESS_PROFILE").is_ok_and(|value| value.trim() == "lean")
}
