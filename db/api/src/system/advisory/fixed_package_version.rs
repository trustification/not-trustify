use crate::system::advisory::advisory_vulnerability::AdvisoryVulnerabilityContext;
use crate::system::advisory::AdvisoryContext;
use huevos_entity::fixed_package_version;
use std::fmt::{Debug, Formatter};

#[derive(Clone)]
pub struct FixedPackageVersionContext {
    pub(crate) advisory_vulnerability: AdvisoryVulnerabilityContext,
    pub(crate) fixed_package_version: fixed_package_version::Model,
}

impl Debug for FixedPackageVersionContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.fixed_package_version.fmt(f)
    }
}

impl From<(&AdvisoryVulnerabilityContext, fixed_package_version::Model)>
    for FixedPackageVersionContext
{
    fn from(
        (advisory_vulnerability, fixed_package_version): (
            &AdvisoryVulnerabilityContext,
            fixed_package_version::Model,
        ),
    ) -> Self {
        Self {
            advisory_vulnerability: advisory_vulnerability.clone(),
            fixed_package_version,
        }
    }
}
