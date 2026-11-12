/// A plan or a curve that breaks its bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("invalid automation curve: {0}")]
    Curve(&'static str),
    #[error("invalid mix plan: {0}")]
    Plan(&'static str),
}
