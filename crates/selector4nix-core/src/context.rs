use crate::application::usecase::dashboard::{
    GetDashboardCacheStatsUseCase, GetDashboardConfigSummaryUseCase, GetDashboardOverviewUseCase,
    GetDashboardTransferringUseCase,
};
use crate::application::usecase::derivation::GetDerivationLogUseCase;
use crate::application::usecase::nar_file::StreamNarFileUseCase;
use crate::application::usecase::nar_info::{ListNarInnerDirectoryUseCase, ResolveNarInfoUseCase};
use crate::application::usecase::sni_proxy::AddSniProxyUseCase;
use crate::application::usecase::substituter::{
    AddSubstituterUseCase, DisableSubstituterUseCase, EnableSubstituterUseCase,
};
use crate::infrastructure::config::CacheInfoConfiguration;

pub struct AppContext {
    pub get_derivation_log_usecase: GetDerivationLogUseCase,
    pub resolve_nar_info_usecase: ResolveNarInfoUseCase,
    pub list_nar_inner_directory_usecase: ListNarInnerDirectoryUseCase,
    pub stream_nar_file_usecase: StreamNarFileUseCase,
    pub get_dashboard_overview_usecase: GetDashboardOverviewUseCase,
    pub get_dashboard_transferring_usecase: GetDashboardTransferringUseCase,
    pub get_dashboard_cache_stats_usecase: GetDashboardCacheStatsUseCase,
    pub get_dashboard_config_summary_usecase: GetDashboardConfigSummaryUseCase,
    pub add_substituter_usecase: AddSubstituterUseCase,
    pub enable_substituter_usecase: EnableSubstituterUseCase,
    pub disable_substituter_usecase: DisableSubstituterUseCase,
    pub add_sni_proxy_usecase: AddSniProxyUseCase,
    pub cache_info: CacheInfoConfiguration,
}
