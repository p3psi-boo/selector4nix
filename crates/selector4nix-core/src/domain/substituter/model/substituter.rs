use getset::Getters;
use tokio::time::Instant;

use crate::domain::common::url::Url;
use crate::domain::substituter::model::{Availability, Priority, SubstituterMeta};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Getters)]
#[getset(get = "pub")]
pub struct Substituter {
    target: SubstituterMeta,
    availability: Availability,
    #[getset(skip)]
    enabled: bool,
}

impl Substituter {
    pub fn new(target: SubstituterMeta, availability: Availability) -> Self {
        Self {
            target,
            availability,
            enabled: true,
        }
    }

    pub fn url(&self) -> &Url {
        self.target.url()
    }

    pub fn priority(&self) -> Priority {
        self.target.priority()
    }

    pub fn prev_failures(&self) -> usize {
        self.availability.prev_failures()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn is_normal(&self) -> bool {
        matches!(&self.availability, Availability::Normal)
    }

    pub fn is_maybe_ready(&self) -> bool {
        matches!(&self.availability, Availability::MaybeReady { .. })
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(
            &self.availability,
            Availability::Offline { .. } | Availability::ServiceError { .. },
        )
    }

    /// Whether this substituter may be selected for NAR info or NAR file traffic.
    pub fn is_selectable(&self) -> bool {
        self.enabled && !self.is_unavailable()
    }

    pub fn enable(
        mut self,
        periodic_probing: PeriodicProbingOption,
    ) -> (Self, Vec<UpdateSubstituterEvent>) {
        if self.enabled {
            return (self, Vec::new());
        }
        self.enabled = true;
        self.availability = Availability::Normal;
        let mut events = vec![UpdateSubstituterEvent::NotifyEnabled];
        if periodic_probing == PeriodicProbingOption::Enabled {
            events.push(UpdateSubstituterEvent::ScheduleProbing(None));
        }
        (self, events)
    }

    pub fn disable(mut self) -> (Self, Vec<UpdateSubstituterEvent>) {
        if !self.enabled {
            return (self, Vec::new());
        }
        self.enabled = false;
        (self, vec![UpdateSubstituterEvent::NotifyDisabled])
    }

    pub fn update_on_service_successful(mut self) -> (Self, Vec<UpdateSubstituterEvent>) {
        if !self.enabled {
            return (self, Vec::new());
        }
        self.availability = self.availability.try_change_to_normal();
        let events = if !self.is_unavailable() {
            vec![UpdateSubstituterEvent::NotifyAvailable]
        } else {
            Vec::new()
        };
        (self, events)
    }

    pub fn update_on_service_offline(
        mut self,
        now: Instant,
    ) -> (Substituter, Vec<UpdateSubstituterEvent>) {
        if !self.enabled || self.is_unavailable() {
            (self, Vec::new())
        } else {
            self.availability = self.availability.try_change_to_offline(now);
            let retry_instant = now + self.availability.retry_duration().unwrap();
            let events = vec![
                UpdateSubstituterEvent::NotifyUnavailable,
                UpdateSubstituterEvent::ScheduleRetryReady(retry_instant),
            ];
            (self, events)
        }
    }

    pub fn update_on_service_error(
        mut self,
        now: Instant,
    ) -> (Substituter, Vec<UpdateSubstituterEvent>) {
        if !self.enabled || self.is_unavailable() {
            (self, Vec::new())
        } else {
            self.availability = self.availability.try_change_to_service_error(now);
            let retry_instant = now + self.availability.retry_duration().unwrap();
            let events = vec![
                UpdateSubstituterEvent::NotifyUnavailable,
                UpdateSubstituterEvent::ScheduleRetryReady(retry_instant),
            ];
            (self, events)
        }
    }

    pub fn update_on_next_retry_ready(mut self) -> (Substituter, Vec<UpdateSubstituterEvent>) {
        if !self.enabled {
            return (self, Vec::new());
        }
        self.availability = self.availability.try_change_to_maybe_ready();
        let events = vec![UpdateSubstituterEvent::ScheduleProbing(None)];
        (self, events)
    }

    pub fn update_on_probing_finished(
        mut self,
        probed_state: ProbedState,
        periodic_probing: PeriodicProbingOption,
        now: Instant,
    ) -> (Substituter, Vec<UpdateSubstituterEvent>) {
        if !self.enabled {
            return (self, Vec::new());
        }
        match probed_state {
            ProbedState::Normal => {
                if self.is_unavailable() {
                    (self, Vec::new())
                } else {
                    self.availability = self.availability.try_change_to_maybe_ready();
                    let events = match periodic_probing {
                        PeriodicProbingOption::Enabled => vec![
                            UpdateSubstituterEvent::NotifyAvailable,
                            UpdateSubstituterEvent::ScheduleProbing(Some(
                                now + Availability::REPROBING_PERIOD,
                            )),
                        ],
                        PeriodicProbingOption::None => {
                            vec![UpdateSubstituterEvent::NotifyAvailable]
                        }
                    };
                    (self, events)
                }
            }
            ProbedState::Offline => self.update_on_service_offline(now),
            ProbedState::ServiceError => self.update_on_service_error(now),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UpdateSubstituterEvent {
    ScheduleRetryReady(Instant),
    ScheduleProbing(Option<Instant>),
    NotifyUnavailable,
    NotifyAvailable,
    NotifyEnabled,
    NotifyDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbedState {
    Normal,
    Offline,
    ServiceError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeriodicProbingOption {
    Enabled,
    None,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use crate::domain::substituter::model::test_support::make_substituter_meta;

    use super::*;

    fn make_substituter(availability: Availability) -> Substituter {
        Substituter::new(make_substituter_meta(), availability)
    }

    fn assert_events_eq(
        actual: impl IntoIterator<Item = UpdateSubstituterEvent>,
        expected: impl IntoIterator<Item = UpdateSubstituterEvent>,
    ) {
        assert_eq!(
            actual.into_iter().collect::<HashSet<_>>(),
            expected.into_iter().collect::<HashSet<_>>(),
        );
    }

    #[test]
    fn update_on_service_successful_given_maybe_ready() {
        let substituter = make_substituter(Availability::MaybeReady { prev_failures: 0 });
        let (result, events) = substituter.update_on_service_successful();
        assert!(!result.is_unavailable());
        assert_events_eq(events, vec![UpdateSubstituterEvent::NotifyAvailable]);
    }

    #[test]
    fn update_on_service_failed_changes_state_from_normal() {
        let substituter = make_substituter(Availability::Normal);
        let now = Instant::now();

        let (result, events) = substituter.update_on_service_error(now);

        assert!(result.is_unavailable());
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyUnavailable,
                UpdateSubstituterEvent::ScheduleRetryReady(now + Duration::from_millis(500)),
            ],
        );
    }

    #[test]
    fn update_on_service_error_increments_backoff_given_repeated_error() {
        let substituter = make_substituter(Availability::MaybeReady { prev_failures: 2 });
        let now = Instant::now();

        let (result, events) = substituter.update_on_service_error(now);

        assert!(result.is_unavailable());
        assert!(matches!(
            result.availability(),
            Availability::ServiceError {
                prev_failures: 3,
                ..
            }
        ));
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyUnavailable,
                UpdateSubstituterEvent::ScheduleRetryReady(now + Duration::from_millis(4000)),
            ],
        );
    }

    #[test]
    fn update_on_next_retry_ready_succeeds() {
        let substituter = make_substituter(Availability::ServiceError {
            detected_at: Instant::now(),
            prev_failures: 0,
        });

        let (result, events) = substituter.update_on_next_retry_ready();

        assert!(!result.is_unavailable());
        assert_events_eq(events, vec![UpdateSubstituterEvent::ScheduleProbing(None)]);
    }

    #[test]
    fn update_on_probing_finished_succeeds_given_probed_state_normal() {
        let substituter = make_substituter(Availability::MaybeReady { prev_failures: 0 });
        let now = Instant::now();

        let (result, events) = substituter.update_on_probing_finished(
            ProbedState::Normal,
            PeriodicProbingOption::Enabled,
            now,
        );

        assert!(!result.is_unavailable());
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyAvailable,
                UpdateSubstituterEvent::ScheduleProbing(Some(now + Availability::REPROBING_PERIOD)),
            ],
        );
    }

    #[test]
    fn update_on_probing_finished_schedules_reprobing_given_already_normal() {
        let substituter = make_substituter(Availability::Normal);
        let now = Instant::now();

        let (result, events) = substituter.update_on_probing_finished(
            ProbedState::Normal,
            PeriodicProbingOption::Enabled,
            now,
        );

        assert!(result.is_normal());
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyAvailable,
                UpdateSubstituterEvent::ScheduleProbing(Some(now + Availability::REPROBING_PERIOD)),
            ],
        );
    }

    #[test]
    fn update_on_probing_finished_emits_unavailable_given_offline() {
        let substituter = make_substituter(Availability::MaybeReady { prev_failures: 0 });
        let now = Instant::now();

        let (result, events) = substituter.update_on_probing_finished(
            ProbedState::Offline,
            PeriodicProbingOption::Enabled,
            now,
        );

        assert!(result.is_unavailable());
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyUnavailable,
                UpdateSubstituterEvent::ScheduleRetryReady(
                    now + Availability::OFFLINE_RETRY_PERIOD,
                ),
            ],
        );
    }

    #[test]
    fn update_on_probing_finished_emits_unavailable_given_service_error() {
        let substituter = make_substituter(Availability::MaybeReady { prev_failures: 2 });
        let now = Instant::now();

        let (result, events) = substituter.update_on_probing_finished(
            ProbedState::ServiceError,
            PeriodicProbingOption::Enabled,
            now,
        );

        assert!(result.is_unavailable());
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyUnavailable,
                UpdateSubstituterEvent::ScheduleRetryReady(now + Duration::from_millis(4000)),
            ],
        );
    }

    #[test]
    fn new_substituter_is_enabled_and_selectable() {
        let substituter = make_substituter(Availability::Normal);
        assert!(substituter.is_enabled());
        assert!(substituter.is_selectable());
    }

    #[test]
    fn disable_removes_selectability_and_emits_event() {
        let substituter = make_substituter(Availability::Normal);
        let (result, events) = substituter.disable();

        assert!(!result.is_enabled());
        assert!(!result.is_selectable());
        assert!(result.is_normal());
        assert_events_eq(events, vec![UpdateSubstituterEvent::NotifyDisabled]);
    }

    #[test]
    fn disable_is_idempotent() {
        let substituter = make_substituter(Availability::Normal);
        let (substituter, _) = substituter.disable();
        let (result, events) = substituter.disable();

        assert!(!result.is_enabled());
        assert!(events.is_empty());
    }

    #[test]
    fn enable_restores_normal_availability() {
        let substituter = make_substituter(Availability::Offline {
            detected_at: Instant::now(),
        });
        let (substituter, _) = substituter.disable();
        let (result, events) = substituter.enable(PeriodicProbingOption::None);

        assert!(result.is_enabled());
        assert!(result.is_normal());
        assert!(result.is_selectable());
        assert_events_eq(events, vec![UpdateSubstituterEvent::NotifyEnabled]);
    }

    #[test]
    fn enable_schedules_probing_when_periodic_probing_is_enabled() {
        let substituter = make_substituter(Availability::Normal);
        let (substituter, _) = substituter.disable();
        let (result, events) = substituter.enable(PeriodicProbingOption::Enabled);

        assert!(result.is_selectable());
        assert_events_eq(
            events,
            vec![
                UpdateSubstituterEvent::NotifyEnabled,
                UpdateSubstituterEvent::ScheduleProbing(None),
            ],
        );
    }

    #[test]
    fn enable_is_idempotent() {
        let substituter = make_substituter(Availability::Normal);
        let (result, events) = substituter.enable(PeriodicProbingOption::Enabled);

        assert!(result.is_enabled());
        assert!(events.is_empty());
    }

    #[test]
    fn disabled_substituter_ignores_health_updates() {
        let substituter = make_substituter(Availability::Normal);
        let (substituter, _) = substituter.disable();
        let now = Instant::now();

        let (after_success, success_events) = substituter.clone().update_on_service_successful();
        let (after_error, error_events) = substituter.clone().update_on_service_error(now);
        let (after_probe, probe_events) = substituter.update_on_probing_finished(
            ProbedState::Normal,
            PeriodicProbingOption::Enabled,
            now,
        );

        assert!(!after_success.is_enabled());
        assert!(after_success.is_normal());
        assert!(success_events.is_empty());
        assert!(error_events.is_empty());
        assert!(!after_error.is_unavailable());
        assert!(probe_events.is_empty());
        assert!(!after_probe.is_unavailable());
    }

    #[test]
    fn offline_substituter_is_not_selectable_even_when_enabled() {
        let substituter = make_substituter(Availability::Offline {
            detected_at: Instant::now(),
        });
        assert!(substituter.is_enabled());
        assert!(!substituter.is_selectable());
    }
}
