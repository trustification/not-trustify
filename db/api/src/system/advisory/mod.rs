//! Support for advisories.

use crate::db::Transactional;
use crate::system::advisory::advisory_vulnerability::AdvisoryVulnerabilityContext;
use crate::system::error::Error;
use crate::system::InnerSystem;
use affected_package_version_range::AffectedPackageVersionRangeContext;
use fixed_package_version::FixedPackageVersionContext;
use huevos_common::advisory::{AdvisoryVulnerabilityAssertions, Assertion};
use huevos_common::purl::Purl;
use huevos_entity as entity;
use migration::m0000032_create_advisory_vulnerability::AdvisoryVulnerability;
use not_affected_package_version::NotAffectedPackageVersionContext;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, EntityTrait, FromQueryResult, QueryFilter};
use sea_orm::{ColumnTrait, QuerySelect, RelationTrait};
use sea_query::{Condition, JoinType};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};

pub mod advisory_vulnerability;

pub mod affected_package_version_range;
pub mod fixed_package_version;
pub mod not_affected_package_version;

pub mod csaf;

impl InnerSystem {
    pub(crate) async fn get_advisory_by_id(
        &self,
        id: i32,
        tx: Transactional<'_>,
    ) -> Result<Option<AdvisoryContext>, Error> {
        Ok(entity::advisory::Entity::find_by_id(id)
            .one(&self.connection(tx))
            .await?
            .map(|advisory| (self, advisory).into()))
    }

    pub async fn get_advisory(
        &self,
        identifier: &str,
        location: &str,
        sha256: &str,
    ) -> Result<Option<AdvisoryContext>, Error> {
        Ok(entity::advisory::Entity::find()
            .filter(Condition::all().add(entity::advisory::Column::Identifier.eq(identifier)))
            .filter(Condition::all().add(entity::advisory::Column::Location.eq(location)))
            .filter(Condition::all().add(entity::advisory::Column::Sha256.eq(sha256.to_string())))
            .one(&self.db)
            .await?
            .map(|sbom| (self, sbom).into()))
    }

    pub async fn ingest_advisory(
        &self,
        identifer: &str,
        location: &str,
        sha256: &str,
        tx: Transactional<'_>,
    ) -> Result<AdvisoryContext, Error> {
        if let Some(found) = self.get_advisory(identifer, location, sha256).await? {
            return Ok(found);
        }

        let model = entity::advisory::ActiveModel {
            identifier: Set(identifer.to_string()),
            location: Set(location.to_string()),
            sha256: Set(sha256.to_string()),
            ..Default::default()
        };

        Ok((self, model.insert(&self.db).await?).into())
    }
}

#[derive(Clone)]
pub struct AdvisoryContext {
    system: InnerSystem,
    advisory: entity::advisory::Model,
}

impl PartialEq for AdvisoryContext {
    fn eq(&self, other: &Self) -> bool {
        self.advisory.eq(&other.advisory)
    }
}

impl Debug for AdvisoryContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.advisory.fmt(f)
    }
}

impl From<(&InnerSystem, entity::advisory::Model)> for AdvisoryContext {
    fn from((system, advisory): (&InnerSystem, entity::advisory::Model)) -> Self {
        Self {
            system: system.clone(),
            advisory,
        }
    }
}

impl AdvisoryContext {
    pub async fn get_vulnerability(
        &self,
        identifier: &str,
        tx: Transactional<'_>,
    ) -> Result<Option<AdvisoryVulnerabilityContext>, Error> {
        Ok(entity::advisory_vulnerability::Entity::find()
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Vulnerability.def(),
            )
            .filter(entity::advisory_vulnerability::Column::AdvisoryId.eq(self.advisory.id))
            .filter(entity::vulnerability::Column::Identifier.eq(identifier))
            .one(&self.system.connection(tx))
            .await?
            .map(|vuln| (self, vuln).into()))
    }

    pub async fn ingest_vulnerability(
        &self,
        identifier: &str,
        tx: Transactional<'_>,
    ) -> Result<AdvisoryVulnerabilityContext, Error> {
        if let Some(found) = self.get_vulnerability(identifier, tx).await? {
            return Ok(found);
        }

        let cve = self.system.ingest_vulnerability(identifier, tx).await?;

        let entity = entity::advisory_vulnerability::ActiveModel {
            advisory_id: Set(self.advisory.id),
            vulnerability_id: Set(cve.cve.id),
        };

        Ok((self, entity.insert(&self.system.connection(tx)).await?).into())
    }

    pub async fn vulnerabilities(
        &self,
        tx: Transactional<'_>,
    ) -> Result<Vec<AdvisoryVulnerabilityContext>, Error> {
        Ok(entity::advisory_vulnerability::Entity::find()
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Vulnerability
                    .def()
                    .rev(),
            )
            .filter(entity::advisory_vulnerability::Column::AdvisoryId.eq(self.advisory.id))
            .all(&self.system.connection(tx))
            .await?
            .drain(0..)
            .map(|e| (self, e).into())
            .collect())
    }

    pub async fn vulnerability_assertions(
        &self,
        tx: Transactional<'_>,
    ) -> Result<AdvisoryVulnerabilityAssertions, Error> {
        let affected = self.affected_assertions(tx).await?;
        let not_affected = self.not_affected_assertions(tx).await?;
        let fixed = self.fixed_assertions(tx).await?;

        let mut merged = affected.assertions.clone();

        for (package_key, assertions) in not_affected.assertions {
            merged
                .entry(package_key)
                .or_insert(Vec::default())
                .extend_from_slice(&assertions)
        }

        for (package_key, assertions) in fixed.assertions {
            merged
                .entry(package_key)
                .or_insert(Vec::default())
                .extend_from_slice(&assertions)
        }

        Ok(AdvisoryVulnerabilityAssertions { assertions: merged })
    }

    pub async fn affected_assertions(
        &self,
        tx: Transactional<'_>,
    ) -> Result<AdvisoryVulnerabilityAssertions, Error> {
        #[derive(FromQueryResult, Debug)]
        struct AffectedVersion {
            ty: String,
            namespace: Option<String>,
            name: String,
            start: String,
            end: String,
            vulnerability: String,
            identifier: String,
            location: String,
            sha256: String,
        }

        let mut affected_version_ranges = entity::affected_package_version_range::Entity::find()
            .column_as(entity::package::Column::Type, "ty")
            .column_as(entity::package::Column::Namespace, "namespace")
            .column_as(entity::package::Column::Name, "name")
            .column_as(entity::package_version_range::Column::Start, "start")
            .column_as(entity::package_version_range::Column::End, "end")
            .column_as(entity::vulnerability::Column::Identifier, "vulnerability")
            .column_as(entity::advisory::Column::Identifier, "identifier")
            .column_as(entity::advisory::Column::Location, "location")
            .column_as(entity::advisory::Column::Sha256, "sha256")
            .join(
                JoinType::Join,
                entity::affected_package_version_range::Relation::PackageVersionRange.def(),
            )
            .join(
                JoinType::Join,
                entity::affected_package_version_range::Relation::Advisory.def(),
            )
            .join(
                JoinType::Join,
                entity::package_version_range::Relation::Package.def(),
            )
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Advisory
                    .def()
                    .rev(),
            )
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Vulnerability.def(),
            )
            .filter(entity::affected_package_version_range::Column::AdvisoryId.eq(self.advisory.id))
            .into_model::<AffectedVersion>()
            .all(&self.system.connection(tx))
            .await?;

        let mut assertions = HashMap::new();

        for each in affected_version_ranges {
            let package_key = Purl {
                ty: each.ty,
                namespace: each.namespace,
                name: each.name,
                version: None,
                qualifiers: Default::default(),
            }
            .to_string();

            let mut package_assertions = assertions.entry(package_key.clone()).or_insert(vec![]);

            package_assertions.push(Assertion::Affected {
                start_version: each.start,
                end_version: each.end,
                vulnerability: each.vulnerability,
            });
        }

        Ok(AdvisoryVulnerabilityAssertions { assertions })
    }

    pub async fn not_affected_assertions(
        &self,
        tx: Transactional<'_>,
    ) -> Result<AdvisoryVulnerabilityAssertions, Error> {
        #[derive(FromQueryResult, Debug)]
        struct NotAffectedVersion {
            ty: String,
            namespace: Option<String>,
            name: String,
            version: String,
            vulnerability: String,
            identifier: String,
            location: String,
            sha256: String,
        }

        let mut not_affected_versions = entity::not_affected_package_version::Entity::find()
            .column_as(entity::package::Column::Type, "ty")
            .column_as(entity::package::Column::Namespace, "namespace")
            .column_as(entity::package::Column::Name, "name")
            .column_as(entity::package_version::Column::Version, "version")
            .column_as(entity::vulnerability::Column::Identifier, "vulnerability")
            .column_as(entity::advisory::Column::Identifier, "identifier")
            .column_as(entity::advisory::Column::Location, "location")
            .column_as(entity::advisory::Column::Sha256, "sha256")
            .join(
                JoinType::Join,
                entity::not_affected_package_version::Relation::PackageVersion.def(),
            )
            .join(
                JoinType::Join,
                entity::not_affected_package_version::Relation::Advisory.def(),
            )
            .join(
                JoinType::Join,
                entity::package_version::Relation::Package.def(),
            )
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Advisory
                    .def()
                    .rev(),
            )
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Vulnerability.def(),
            )
            .filter(entity::not_affected_package_version::Column::AdvisoryId.eq(self.advisory.id))
            .into_model::<NotAffectedVersion>()
            .all(&self.system.connection(tx))
            .await?;

        let mut assertions = HashMap::new();

        for each in not_affected_versions {
            let package_key = Purl {
                ty: each.ty,
                namespace: each.namespace,
                name: each.name,
                version: None,
                qualifiers: Default::default(),
            }
            .to_string();

            let mut package_assertions = assertions.entry(package_key.clone()).or_insert(vec![]);

            package_assertions.push(Assertion::NotAffected {
                vulnerability: each.vulnerability,
                version: each.version,
            });
        }

        Ok(AdvisoryVulnerabilityAssertions { assertions })
    }

    pub async fn fixed_assertions(
        &self,
        tx: Transactional<'_>,
    ) -> Result<AdvisoryVulnerabilityAssertions, Error> {
        #[derive(FromQueryResult, Debug)]
        struct FixedVersion {
            ty: String,
            namespace: Option<String>,
            name: String,
            version: String,
            vulnerability: String,
            identifier: String,
            location: String,
            sha256: String,
        }

        let mut fixed_versions = entity::fixed_package_version::Entity::find()
            .column_as(entity::package::Column::Type, "ty")
            .column_as(entity::package::Column::Namespace, "namespace")
            .column_as(entity::package::Column::Name, "name")
            .column_as(entity::package_version::Column::Version, "version")
            .column_as(entity::vulnerability::Column::Identifier, "vulnerability")
            .column_as(entity::advisory::Column::Identifier, "identifier")
            .column_as(entity::advisory::Column::Location, "location")
            .column_as(entity::advisory::Column::Sha256, "sha256")
            .join(
                JoinType::Join,
                entity::fixed_package_version::Relation::PackageVersion.def(),
            )
            .join(
                JoinType::Join,
                entity::fixed_package_version::Relation::Advisory.def(),
            )
            .join(
                JoinType::Join,
                entity::package_version::Relation::Package.def(),
            )
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Advisory
                    .def()
                    .rev(),
            )
            .join(
                JoinType::Join,
                entity::advisory_vulnerability::Relation::Vulnerability.def(),
            )
            .filter(entity::fixed_package_version::Column::AdvisoryId.eq(self.advisory.id))
            .into_model::<FixedVersion>()
            .all(&self.system.connection(tx))
            .await?;

        let mut assertions = HashMap::new();

        for each in fixed_versions {
            let package_key = Purl {
                ty: each.ty,
                namespace: each.namespace,
                name: each.name,
                version: None,
                qualifiers: Default::default(),
            }
            .to_string();

            let mut package_assertions = assertions.entry(package_key.clone()).or_insert(vec![]);

            package_assertions.push(Assertion::Fixed {
                vulnerability: each.vulnerability,
                version: each.version,
            });
        }

        Ok(AdvisoryVulnerabilityAssertions { assertions })
    }
}

#[cfg(test)]
mod test {
    use crate::db::Transactional;
    use crate::system::InnerSystem;
    use huevos_common::advisory::Assertion;
    use std::collections::HashSet;

    #[tokio::test]
    async fn ingest_advisories() -> Result<(), anyhow::Error> {
        let system = InnerSystem::for_test("ingest_advisories").await?;

        let advisory1 = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        let advisory2 = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        let advisory3 = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "89",
                Transactional::None,
            )
            .await?;

        assert_eq!(advisory1.advisory.id, advisory2.advisory.id);
        assert_ne!(advisory1.advisory.id, advisory3.advisory.id);

        Ok(())
    }

    #[tokio::test]
    async fn ingest_affected_package_version_range() -> Result<(), anyhow::Error> {
        let system = InnerSystem::for_test("ingest_affected_package_version_range").await?;

        let advisory = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        let advisory_vulnerability = advisory
            .ingest_vulnerability("CVE-8675309", Transactional::None)
            .await?;

        let affected1 = advisory_vulnerability
            .ingest_affected_package_range(
                "pkg://maven/io.quarkus/quarkus-core",
                "1.0.2",
                "1.2.0",
                Transactional::None,
            )
            .await?;

        let affected2 = advisory_vulnerability
            .ingest_affected_package_range(
                "pkg://maven/io.quarkus/quarkus-core",
                "1.0.2",
                "1.2.0",
                Transactional::None,
            )
            .await?;

        let affected3 = advisory_vulnerability
            .ingest_affected_package_range(
                "pkg://maven/io.quarkus/quarkus-addons",
                "1.0.2",
                "1.2.0",
                Transactional::None,
            )
            .await?;

        assert_eq!(
            affected1.affected_package_version_range.id,
            affected2.affected_package_version_range.id
        );
        assert_ne!(
            affected1.affected_package_version_range.id,
            affected3.affected_package_version_range.id
        );

        Ok(())
    }

    #[tokio::test]
    async fn ingest_fixed_package_version() -> Result<(), anyhow::Error> {
        let system = InnerSystem::for_test("ingest_fixed_package_version").await?;

        let advisory = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        let advisory_vulnerability = advisory
            .ingest_vulnerability("CVE-1234567", Transactional::None)
            .await?;

        let affected = advisory_vulnerability
            .ingest_affected_package_range(
                "pkg://maven/io.quarkus/quarkus-core",
                "1.0.2",
                "1.2.0",
                Transactional::None,
            )
            .await?;

        let fixed1 = advisory_vulnerability
            .ingest_fixed_package_version(
                "pkg://maven/io.quarkus/quarkus-core@1.2.0",
                Transactional::None,
            )
            .await?;

        let fixed2 = advisory_vulnerability
            .ingest_fixed_package_version(
                "pkg://maven/io.quarkus/quarkus-core@1.2.0",
                Transactional::None,
            )
            .await?;

        let fixed3 = advisory_vulnerability
            .ingest_fixed_package_version(
                "pkg://maven/io.quarkus/quarkus-addons@1.2.0",
                Transactional::None,
            )
            .await?;

        assert_eq!(
            fixed1.fixed_package_version.id,
            fixed2.fixed_package_version.id
        );
        assert_ne!(
            fixed1.fixed_package_version.id,
            fixed3.fixed_package_version.id
        );

        Ok(())
    }

    #[tokio::test]
    async fn ingest_advisory_cve() -> Result<(), anyhow::Error> {
        let system = InnerSystem::for_test("ingest_advisory_cve").await?;

        let advisory = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        advisory
            .ingest_vulnerability("CVE-123", Transactional::None)
            .await?;
        advisory
            .ingest_vulnerability("CVE-123", Transactional::None)
            .await?;
        advisory
            .ingest_vulnerability("CVE-456", Transactional::None)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn advisory_affected_vulnerability_assertions() -> Result<(), anyhow::Error> {
        /*
        env_logger::builder()
            .filter_level(log::LevelFilter::Info)
            .is_test(true)
            .init();

         */

        let system = InnerSystem::for_test("advisory_affected_vulnerability_assertions").await?;

        let advisory = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        let advisory_vulnerability = advisory
            .ingest_vulnerability("CVE-42", Transactional::None)
            .await?;

        advisory_vulnerability
            .ingest_affected_package_range(
                "pkg://maven/io.quarkus/quarkus-core",
                "1.0.2",
                "1.2.0",
                Transactional::None,
            )
            .await?;

        advisory_vulnerability
            .ingest_not_affected_package_version(
                "pkg://maven/.io.quarkus/quarkus-core@1.1.9",
                Transactional::None,
            )
            .await?;

        let affected = advisory.affected_assertions(Transactional::None).await?;

        assert_eq!(1, affected.assertions.len());

        Ok(())
    }

    #[tokio::test]
    async fn advisory_not_affected_vulnerability_assertions() -> Result<(), anyhow::Error> {
        /*
        env_logger::builder()
            .filter_level(log::LevelFilter::Info)
            .is_test(true)
            .init();

         */

        let system =
            InnerSystem::for_test("advisory_not_affected_vulnerability_assertions").await?;

        let advisory = system
            .ingest_advisory(
                "RHSA-GHSA-1",
                "http://db.com/rhsa-ghsa-2",
                "2",
                Transactional::None,
            )
            .await?;

        let advisory_vulnerability = advisory
            .ingest_vulnerability("INTERAL-77", Transactional::None)
            .await?;

        advisory_vulnerability
            .ingest_affected_package_range(
                "pkg://maven/io.quarkus/quarkus-core",
                "1.0.2",
                "1.2.0",
                Transactional::None,
            )
            .await?;

        advisory_vulnerability
            .ingest_not_affected_package_version(
                "pkg://maven/io.quarkus/quarkus-core@1.1.9",
                Transactional::None,
            )
            .await?;

        let not_affected = advisory
            .not_affected_assertions(Transactional::None)
            .await?;

        assert_eq!(1, not_affected.assertions.len());

        let pkg_assertions = not_affected
            .assertions
            .get(&"pkg://maven/io.quarkus/quarkus-core".to_string());

        assert!(pkg_assertions.is_some());

        let pkg_assertions = pkg_assertions.unwrap();

        assert_eq!(1, pkg_assertions.len());

        let assertion = &pkg_assertions[0];

        assert!(matches!( assertion, Assertion::NotAffected {version, ..}
            if version == "1.1.9"
        ));

        Ok(())
    }
}
