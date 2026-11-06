//! A server's menu whose type Horizon has no screen for: a Fabric mod's menu
//! type (synced over `fabric:registry/sync`, so its id resolves to the mod's
//! name), or an `ExtendedMenuType` opened through Fabric API's
//! `fabric-menu-api-v1:open_screen` payload in place of the vanilla
//! `ClientboundOpenScreenPacket` (horizon_protocol::fabric::menu).
//!
//! The server holds such a menu open until the client closes it, and while
//! it does it drops every click on the player's own inventory
//! (`handleContainerClick` checks the window id). A Fabric client builds the
//! mod's screen from its menu type; Horizon has no code of the mod, so it
//! waits for the window's first `ContainerSetContent` and looks at its size:
//! the slots of a vanilla chest or hopper (`ChestMenu`, `HopperMenu`: the
//! container's slots, then the player's 27 + 9) show as that vanilla screen,
//! whose slot numbers are the server's, so every click lands where the server
//! expects it. Any other menu is closed again (`ServerboundContainerClosePacket`)
//! with a line in chat.
use super::{Game, Screen};
use horizon_protocol::{
    fabric::menu,
    packets::play::{self, Serverbound as S},
    registry::Ids,
};
use horizon_world::nbt::Tag;

/// A mod's menu the server opened, waiting for its slots.
pub(super) struct PendingMenu {
    container: i32,
    menu: String,
    title: Tag,
}

/// What becomes of a mod's menu.
#[derive(Debug, PartialEq)]
pub(super) enum MenuDecision {
    /// Shown as this vanilla menu: the `OpenScreen` that opens it.
    Vanilla(play::OpenScreen),
    /// Closed again; the menu's id names it in chat.
    Close { container: i32, menu: String },
}

/// The player's inventory a `ChestMenu` or `HopperMenu` ends with.
const PLAYER_SLOTS: usize = 36;

/// The vanilla menu type a window of `slots` slots has the layout of.
fn vanilla_shape(slots: usize) -> Option<&'static str> {
    match slots.checked_sub(PLAYER_SLOTS)? {
        5 => Some("minecraft:hopper"),
        9 => Some("minecraft:generic_9x1"),
        18 => Some("minecraft:generic_9x2"),
        27 => Some("minecraft:generic_9x3"),
        36 => Some("minecraft:generic_9x4"),
        45 => Some("minecraft:generic_9x5"),
        54 => Some("minecraft:generic_9x6"),
        _ => None,
    }
}

/// The menu shown as the vanilla type of its shape, by that type's id in the
/// server's menu table; closed when it has no such shape or the server no
/// such type.
fn decide(pending: PendingMenu, slots: usize, menus: Option<&Ids>) -> MenuDecision {
    match vanilla_shape(slots).and_then(|name| menus?.id(name)) {
        Some(id) => MenuDecision::Vanilla(play::OpenScreen {
            container: pending.container,
            menu: id,
            title: pending.title,
        }),
        None => MenuDecision::Close {
            container: pending.container,
            menu: pending.menu,
        },
    }
}

impl Game<'_> {
    /// `ClientNetworking.openScreen` for Fabric API's extended menus: the
    /// same as an `OpenScreen` of a menu type Horizon doesn't draw. A payload
    /// that doesn't decode is dropped, as Fabric drops one whose type it
    /// doesn't know.
    pub(super) fn remote_fabric_open_screen(&mut self, data: &[u8]) {
        match menu::decode_open_screen(data) {
            Ok(p) => self.remote_unknown_menu(p.container, p.menu, p.title),
            Err(e) => log::warn!("Fabric: a bad {} payload: {e}", menu::OPEN_SCREEN),
        }
    }

    /// An `OpenScreen` of a menu type Horizon has no screen for. A mod's
    /// waits for its slots; a vanilla type, or an id the server's table
    /// doesn't name, closes at once.
    pub(super) fn remote_unknown_menu(&mut self, container: i32, menu: String, title: Tag) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        if menu.is_empty() || menu.starts_with("minecraft:") {
            r.fabric_menu = None;
            self.remote_menu_decision(MenuDecision::Close { container, menu });
            return;
        }
        // The server replaced any menu still waiting.
        r.fabric_menu = Some(PendingMenu {
            container,
            menu,
            title,
        });
    }

    /// A `ContainerSetContent` before it fills the window: the waiting mod
    /// menu of that window opens or closes now.
    pub(super) fn remote_unknown_menu_content(&mut self, container: i32, slots: usize) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        if r.fabric_menu
            .as_ref()
            .is_none_or(|p| p.container != container)
        {
            return;
        }
        let Some(pending) = r.fabric_menu.take() else {
            return;
        };
        let decision = decide(pending, slots, r.builtin.get("minecraft:menu"));
        self.remote_menu_decision(decision);
    }

    /// The server opened or closed another window: a waiting mod menu is gone.
    pub(super) fn remote_unknown_menu_forget(&mut self) {
        if let Some(r) = self.remote.as_mut() {
            r.fabric_menu = None;
        }
    }

    fn remote_menu_decision(&mut self, decision: MenuDecision) {
        match decision {
            MenuDecision::Vanilla(open) => self.remote_open_screen(open),
            MenuDecision::Close { container, menu } => {
                self.send(S::ContainerClose(play::ContainerClose { container }));
                // The screen open before is the server's no longer.
                if self.screen == Screen::Inventory {
                    self.remote_close_local();
                    self.screen = Screen::Playing;
                    self.capture(true);
                }
                let name = if menu.is_empty() { "?" } else { menu.as_str() };
                self.chat_line(
                    horizon_client::text::Text::tr(
                        "horizon.multiplayer.menu_unsupported",
                        &[&name],
                    ),
                    horizon_client::chat::ChatSource::Client,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vanilla menu table's ids (`registries.json`, `minecraft:menu`),
    /// with a mod's type after them as registry sync numbers it.
    fn menus() -> Ids {
        Ids::from_names(
            [
                "minecraft:generic_9x1",
                "minecraft:generic_9x2",
                "minecraft:generic_9x3",
                "minecraft:generic_9x4",
                "minecraft:generic_9x5",
                "minecraft:generic_9x6",
                "minecraft:generic_3x3",
                "minecraft:crafter_3x3",
                "minecraft:anvil",
                "minecraft:beacon",
                "minecraft:blast_furnace",
                "minecraft:brewing_stand",
                "minecraft:crafting",
                "minecraft:enchantment",
                "minecraft:furnace",
                "minecraft:grindstone",
                "minecraft:hopper",
                "minecraft:lectern",
                "minecraft:loom",
                "minecraft:merchant",
                "minecraft:shulker_box",
                "minecraft:smithing",
                "minecraft:smoker",
                "minecraft:cartography_table",
                "minecraft:stonecutter",
                "backpacks:backpack",
            ]
            .map(String::from)
            .to_vec(),
        )
    }

    fn pending(menu: &str) -> PendingMenu {
        PendingMenu {
            container: 3,
            menu: menu.into(),
            title: Tag::String("Backpack".into()),
        }
    }

    #[test]
    fn a_chest_shaped_mod_menu_shows_as_the_vanilla_chest() {
        let menus = menus();
        // ChestMenu.threeRows: 27 + the player's 36.
        assert_eq!(
            decide(pending("backpacks:backpack"), 63, Some(&menus)),
            MenuDecision::Vanilla(play::OpenScreen {
                container: 3,
                menu: 2,
                title: Tag::String("Backpack".into()),
            })
        );
        for (slots, id) in [(45, 0), (72, 3), (81, 4), (90, 5), (41, 16)] {
            match decide(pending("backpacks:backpack"), slots, Some(&menus)) {
                MenuDecision::Vanilla(open) => assert_eq!(open.menu, id, "{slots} slots"),
                d => panic!("{slots} slots: {d:?}"),
            }
        }
    }

    #[test]
    fn any_other_shape_closes() {
        let menus = menus();
        // An iron chest's 9×9, a machine's 3 + 36, fewer than a player's
        // inventory, an empty window, a size no server could send.
        for slots in [117, 39, 35, 0, usize::MAX] {
            assert_eq!(
                decide(pending("ironchests:iron_chest"), slots, Some(&menus)),
                MenuDecision::Close {
                    container: 3,
                    menu: "ironchests:iron_chest".into()
                },
                "{slots} slots"
            );
        }
    }

    #[test]
    fn a_server_without_the_vanilla_type_closes() {
        assert!(matches!(
            decide(pending("backpacks:backpack"), 63, None),
            MenuDecision::Close { .. }
        ));
        let no_chest = Ids::from_names(vec!["minecraft:hopper".into()]);
        assert!(matches!(
            decide(pending("backpacks:backpack"), 63, Some(&no_chest)),
            MenuDecision::Close { .. }
        ));
    }
}
