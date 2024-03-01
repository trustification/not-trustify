use sea_orm::{DeriveActiveEnum, EnumIter};

#[derive(Debug, Copy, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum Relationship {
    #[sea_orm(num_value = 0)]
    ContainedBy,
    #[sea_orm(num_value = 1)]
    DependencyOf,
    #[sea_orm(num_value = 2)]
    DevDependencyOf,
    #[sea_orm(num_value = 3)]
    OptionalDependencyOf,
    #[sea_orm(num_value = 4)]
    ProvidedDependencyOf,
    #[sea_orm(num_value = 5)]
    TestDependencyOf,
    #[sea_orm(num_value = 6)]
    RuntimeDependencyOf,
    #[sea_orm(num_value = 7)]
    ExampleOf,
    #[sea_orm(num_value = 8)]
    GeneratedFrom,
    #[sea_orm(num_value = 9)]
    AncestorOf,
    #[sea_orm(num_value = 10)]
    VariantOf,
    #[sea_orm(num_value = 11)]
    BuildToolOf,
    #[sea_orm(num_value = 12)]
    DevToolOf,
}
