#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationDiagnostics {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IssueSeverity {
    Error,
    Warning,
}

pub(super) fn append_issue(
    diagnostics: &mut ValidationDiagnostics,
    message: impl Into<String>,
    severity: IssueSeverity,
) {
    match severity {
        IssueSeverity::Error => diagnostics.errors.push(message.into()),
        IssueSeverity::Warning => diagnostics.warnings.push(message.into()),
    }
}

pub(super) fn merge_diagnostics(base: &mut ValidationDiagnostics, incoming: ValidationDiagnostics) {
    base.errors.extend(incoming.errors);
    base.warnings.extend(incoming.warnings);
}
