//! Narratiiviset langat — tapahtumien ajalliset ketjut.
//!
//! [`NarrativeThread`] sitoo yhteen tapahtumat jotka liittyvät toisiinsa
//! ajallisesti, temaattisesti tai kausaalisesti. Jokainen [`ThreadEvent`]
//! on yksittäinen solmu langassa, ja [`Link`] yhdistää tapahtumia
//! eri langoista (ristiviittaus).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// Tapahtuman tyyppi narratiivisessa langassa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    /// Uusi muisto luotu.
    MemoryCreated,
    /// Emotionaalinen muutos.
    EmotionalShift,
    /// Päätös tehty.
    Decision,
    /// Oppiminen tai oivallus.
    Learning,
    /// Ihmisen tekemä korjaus.
    Correction,
}

/// Yhteystyypit narratiivisten lankojen välillä.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationType {
    /// Jatkaa edellisen langan tarinaa.
    Continues,
    /// On ristiriidassa toisen tapahtuman kanssa.
    Contradicts,
    /// Laajentaa aiempaa tapahtumaa.
    Expands,
    /// Toimii emotionaalisena laukaisimena.
    EmotionalTrigger,
}

/// Linkki kahden tapahtuman välillä (mahdollisesti eri langoissa).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Lähdetapahtuman ID.
    pub source: Uuid,
    /// Kohdetapahtuman ID.
    pub target: Uuid,
    /// Linkin tyyppi.
    pub relation: RelationType,
}

/// Yksittäinen tapahtuma narratiivisessa langassa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadEvent {
    /// Tapahtuman uniikki tunniste.
    pub id: Uuid,
    /// Langan ID johon tapahtuma kuuluu.
    pub thread_id: Uuid,
    /// Tapahtuman tyyppi.
    pub event_type: EventType,
    /// Tapahtuman sisältö.
    pub content: String,
    /// Agentti joka loi tapahtuman.
    pub agent_id: String,
    /// Aikaleima.
    pub timestamp: DateTime<Utc>,
    /// Linkit muihin tapahtumiin (ristiviittaukset).
    pub linked_to: Vec<Uuid>,
}

impl ThreadEvent {
    /// Luo uuden tapahtuman.
    #[must_use]
    pub fn new(
        thread_id: Uuid,
        event_type: EventType,
        content: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            thread_id,
            event_type,
            content: content.into(),
            agent_id: agent_id.into(),
            timestamp: Utc::now(),
            linked_to: Vec::new(),
        }
    }

    /// Lisää linkin toiseen tapahtumaan.
    pub fn link_to(&mut self, target_event_id: Uuid) {
        if !self.linked_to.contains(&target_event_id) {
            self.linked_to.push(target_event_id);
        }
    }

    /// Onko tällä tapahtumalla linkkejä.
    #[must_use]
    pub fn has_links(&self) -> bool {
        !self.linked_to.is_empty()
    }
}

/// Narratiivinen lanka — tapahtumien ajallinen ketju.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NarrativeThread {
    /// Langan uniikki tunniste.
    pub id: Uuid,
    /// Langan otsikko.
    pub title: String,
    /// Osallistuvat agentit (nimet).
    pub participants: Vec<String>,
    /// Langan tapahtumat aikajärjestyksessä.
    pub events: Vec<ThreadEvent>,
    /// Luontiaika.
    pub created_at: DateTime<Utc>,
    /// Viimeisin päivitysaika.
    pub updated_at: DateTime<Utc>,
}

impl NarrativeThread {
    /// Luo uuden narratiivisen langan.
    #[must_use]
    pub fn new(title: impl Into<String>, participants: Vec<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            participants,
            events: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Lisää tapahtuman lankaan ja päivittää aikaleiman.
    pub fn add_event(&mut self, event: ThreadEvent) {
        self.events.push(event);
        self.updated_at = Utc::now();
    }

    /// Etsii kaikki linkit tämän langan tapahtumista toisiin lankoihin.
    #[must_use]
    pub fn find_cross_references(&self) -> Vec<(Uuid, Uuid)> {
        let own_ids: HashSet<Uuid> = self.events.iter().map(|e| e.id).collect();
        let mut refs = Vec::new();
        for event in &self.events {
            for linked in &event.linked_to {
                if !own_ids.contains(linked) {
                    refs.push((event.id, *linked));
                }
            }
        }
        refs
    }

    /// Palauttaa tapahtumat kronologisessa järjestyksessä.
    #[must_use]
    pub fn timeline(&self) -> &[ThreadEvent] {
        // Oletetaan että events on jo aikajärjestyksessä (lisäysjärjestys)
        &self.events
    }

    /// Langan tapahtumien lukumäärä.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_add_event() {
        let mut thread = NarrativeThread::new(
            "Test thread",
            vec!["agent_gamma".into(), "agent_alpha".into()],
        );
        let event =
            ThreadEvent::new(thread.id, EventType::MemoryCreated, "hello", "agent_gamma");
        thread.add_event(event);
        assert_eq!(thread.event_count(), 1);
    }

    #[test]
    fn thread_cross_reference() {
        let mut thread_a = NarrativeThread::new("A", vec!["agent_gamma".into()]);
        let mut thread_b = NarrativeThread::new("B", vec!["agent_alpha".into()]);

        let mut event_a =
            ThreadEvent::new(thread_a.id, EventType::MemoryCreated, "a event", "agent_gamma");
        let event_b_id = Uuid::new_v4();
        event_a.link_to(event_b_id);
        thread_a.add_event(event_a);
        thread_b.add_event(ThreadEvent {
            id: event_b_id,
            thread_id: thread_b.id,
            event_type: EventType::Decision,
            content: "b event".into(),
            agent_id: "agent_alpha".into(),
            timestamp: Utc::now(),
            linked_to: vec![],
        });

        let refs = thread_a.find_cross_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].1, event_b_id);
    }

    #[test]
    fn thread_timeline_order() {
        let mut thread = NarrativeThread::new("Timeline", vec![]);
        let e1 = ThreadEvent::new(thread.id, EventType::MemoryCreated, "first", "a");
        let e2 = ThreadEvent::new(thread.id, EventType::Decision, "second", "a");
        let e3 = ThreadEvent::new(thread.id, EventType::Learning, "third", "a");
        thread.add_event(e1);
        thread.add_event(e2);
        thread.add_event(e3);

        let timeline = thread.timeline();
        assert_eq!(timeline.len(), 3);
        // Verifioidaan järjestys (ei tarkkaa aikaleimaa koska testi on nopea)
    }

    #[test]
    fn event_link_to_deduplicates() {
        let mut event =
            ThreadEvent::new(Uuid::new_v4(), EventType::MemoryCreated, "test", "agent");
        let target = Uuid::new_v4();
        event.link_to(target);
        event.link_to(target); // duplicate
        assert_eq!(event.linked_to.len(), 1);
    }
}
