//! What a Fabric server's mods change for every client, ported for Horizon
//! as a client of such a server. Their registry entries come over
//! `fabric:registry/sync` (horizon_protocol::fabric); Horizon announces no
//! play channels, so mods that check for their client half
//! (`NetworkingHelper.isModPresentClientside`) treat it as a vanilla client.
//! What they apply to every player regardless is here.
use super::Game;
use horizon_protocol::packets::play;

/// `state` as placed, with a Fabric server's mod states from `first` on.
fn placed(state: i32, first: Option<u32>) -> u32 {
    horizon_protocol::fabric::state_ids::placed_state(state.max(0) as u32, first)
}

/// Vehicle Upgrade's attribute (`ModRegistry.AIRBORNE_MINING_SPEED_ATTRIBUTE`):
/// `RangedAttribute(0.2, 0, 20)`, synced.
pub(super) const AIRBORNE_MINING_SPEED: &str = "vehicleupgrade:airborne_mining_speed";

impl Game<'_> {
    /// A block state id a packet places (a block update, a falling block, a
    /// break effect): a Fabric server mod's stands as the placeholder
    /// (`fabric::state_ids::PLACEHOLDER_STATE`), a negative one as air.
    pub(super) fn placed_state(&self, state: i32) -> u32 {
        let first = self
            .remote
            .as_ref()
            .and_then(|r| r.chunk_format.modded_states());
        placed(state, first)
    }

    /// A server mod's items, data components and particles can't be read
    /// (`fabric::state_ids`): the packet that holds one is skipped. Said once
    /// per connection, on the first such packet from a server with mods there.
    pub(super) fn remote_fabric_unreadable(&mut self) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        if r.registries.fabric_bounds.is_none() || r.fabric_unreadable_told {
            return;
        }
        r.fabric_unreadable_told = true;
        self.chat_line(
            horizon_client::text::Text::tr("horizon.multiplayer.fabric_unreadable", &[]),
            horizon_client::chat::ChatSource::Client,
        );
    }

    /// The player's own synced attributes that no vanilla attribute holds:
    /// their values, by the name the server's table gives their id.
    pub(super) fn remote_mod_attributes(&mut self, values: &[play::AttributeSnapshot]) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let Some(table) = r.builtin.get("minecraft:attribute") else {
            return;
        };
        for s in values {
            if let Some(name) = table.name(s.attribute)
                && !name.starts_with("minecraft:")
            {
                r.mod_attributes
                    .insert(name.to_owned(), super::remote::snapshot_value(s));
            }
        }
    }

    /// Vehicle Upgrade's `AirborneMiningSpeedHandler.onCalculateBlockBreakSpeed`,
    /// after `Player.getDestroySpeed`'s ÷5 for a player off the ground: the
    /// speed times the attribute × 5. Riding adds a transient ×(1 + 4) modifier
    /// (`vehicleupgrade:riding`, while `removePassengerMiningSpeedMalus` is on),
    /// so a rider mines at ground speed; otherwise the factor is 0.2 × 5 = 1.
    /// None on a server that doesn't sync the attribute.
    pub(super) fn airborne_mining_factor(&self) -> Option<f32> {
        let value = *self
            .remote
            .as_ref()?
            .mod_attributes
            .get(AIRBORNE_MINING_SPEED)?;
        Some(airborne_factor(value))
    }
}

/// `RangedAttribute.sanitizeValue` (0 to 20, NaN to the minimum), then
/// `value * 5.0` as the handler's `(float)` cast.
fn airborne_factor(value: f64) -> f32 {
    let value = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 20.0)
    };
    (value * 5.0) as f32
}

#[cfg(test)]
mod tests {
    use super::placed;

    #[test]
    fn placed_states_from_a_fabric_servers_mods_are_stone() {
        // 26.3's 35723 vanilla states: 35722 is the last (reports/blocks.json).
        assert_eq!(placed(35722, Some(35723)), 35722);
        assert_eq!(placed(35723, Some(35723)), 1);
        assert_eq!(placed(i32::MAX, Some(35723)), 1);
        assert_eq!(placed(-5, Some(35723)), 0);
        assert_eq!(placed(40000, None), 40000);
    }

    use super::*;

    fn snapshot(modifiers: Vec<play::AttributeModifier>) -> play::AttributeSnapshot {
        play::AttributeSnapshot {
            attribute: 40,
            base: 0.2,
            modifiers,
        }
    }

    #[test]
    fn a_rider_mines_at_ground_speed_and_a_jumper_as_in_vanilla() {
        let walking = super::super::remote::snapshot_value(&snapshot(vec![]));
        assert_eq!(airborne_factor(walking), 1.0);
        let riding =
            super::super::remote::snapshot_value(&snapshot(vec![play::AttributeModifier {
                id: "vehicleupgrade:riding".into(),
                amount: 4.0,
                operation: 2,
            }]));
        // 0.2 × (1 + 4) × 5: vanilla's ÷5 undone.
        assert_eq!(airborne_factor(riding), 5.0);
    }

    #[test]
    fn the_attribute_is_kept_in_its_range() {
        assert_eq!(airborne_factor(100.0), 100.0);
        assert_eq!(airborne_factor(-1.0), 0.0);
        assert_eq!(airborne_factor(f64::NAN), 0.0);
    }
}
