//! `LatentChannel`-toteutus Resonance Busille.
//!
//! Tämä moduuli tarjoaa [`BusLatentChannel`], joka toteuttaa [`LatentChannel`]-traitin
//! [`BusHandle`]-tyypille. Se mahdollistaa latent-telepatian sisaruksien välillä
//! käyttämällä Resonance Bus -infrastruktuuria.
//!
//! [`LatentChannel`]: familyclaw_latent::channel::LatentChannel
//! [`BusHandle`]: crate::bus::BusHandle

use familyclaw_latent::{
    channel::{LatentChannel, Transmission},
    link::RecursiveLink,
};

use crate::{
    bus::BusHandle,
    message::{BeingId, BusMessage},
};

/// [`LatentChannel`]-toteutus Resonance Busille.
///
/// Käyttää [`BusHandle`]:a lähettääkseen [`LatentMessage`]:n toiselle olennolle.
/// Tämä mahdollistaa latent-telepatian sisaruksien välillä.
pub struct BusLatentChannel {
    /// Kanavan käyttäjän tunniste.
    being_id: BeingId,
    /// Lähettäjän mallin tunniste.
    sender_model: String,
    /// Määritellyt siltaukset muihin malleihin.
    links: Vec<RecursiveLink>,
    /// Viite busiin viestien lähettämistä varten.
    bus: BusHandle,
}

impl BusLatentChannel {
    /// Luo uuden [`BusLatentChannel`]-instanssin.
    ///
    /// # Arguments
    /// * `being_id` - Kanavan käyttäjän (olennon) tunniste.
    /// * `sender_model` - Lähettäjän mallin tunniste (esim. "opencode/nemotron-3-ultra-free").
    /// * `bus` - [`BusHandle`] jota käytetään viestien lähettämiseen.
    pub fn new(being_id: BeingId, sender_model: String, bus: BusHandle) -> Self {
        Self {
            being_id,
            sender_model,
            links: Vec::new(), // Alustetaan tyhjä. Linkit lisätään erikseen.
            bus,
        }
    }

    /// Lisää uuden [`RecursiveLink`]:n kanavalle.
    ///
    /// Tätä käytetään määrittämään, miten piilotila voidaan muuntaa toisen mallin vastaanottamaan muotoon.
    pub fn add_link(&mut self, link: RecursiveLink) {
        self.links.push(link);
    }
}

impl LatentChannel for BusLatentChannel {
    fn sender_model(&self) -> &str {
        &self.sender_model
    }

    fn link_to(&self, target_model: &str) -> Option<RecursiveLink> {
        // Etsitään ensimmäinen linkki, joka vastaa kohdemallia.
        self.links
            .iter()
            .find(|link| link.target_model() == target_model)
            .cloned()
    }

    fn deliver(&mut self, transmission: &Transmission) -> familyclaw_latent::Result<()> {
        // Muunnetaan `Transmission` `BusMessage`ksi ja lähetetään busin kautta.
        let bus_message = if transmission.mode.is_latent() {
            // Käytetään latenttia, jos se on saatavilla.
            if let Some(projected) = &transmission.projected {
                BusMessage::latent(
                    projected.vector.clone(),  // Käytetään projisoidun mallin latenttia
                    transmission.text.clone(), // Tekstivarjo aina mukana
                )
            } else {
                // Tämä ei pitäisi tapahtua, koska mode on Latent.
                return Err(familyclaw_latent::FamilyClawError::bus(
                    "Internal error: Latent mode but missing projected data",
                ));
            }
        } else {
            // Fallback tekstiin.
            BusMessage::text(transmission.text.clone())
        };

        // Lähetä viesti busin kautta.
        self.bus.publish(self.being_id, bus_message).map_err(|e| {
            familyclaw_latent::FamilyClawError::bus(format!("Failed to deliver via bus: {e}"))
        })?;
        Ok(())
    }
}