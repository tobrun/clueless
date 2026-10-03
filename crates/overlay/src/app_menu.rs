//! The main menu: invisible under the Accessory activation policy, but the
//! only place AppKit reads `cmd` key equivalents from, so `cmd+C`, `cmd+A`,
//! `cmd+W` and `cmd+Q` need items here (spec D-close, D-key-equivalents). Copy and
//! Select All target `nil` and travel the responder chain to the focused
//! text view; Close targets `performClose:` on the key window, which the
//! standard window turns into a hide; Quit targets the status item's handler
//! so it runs the same graceful quit path as the menu-bar item.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{NSString, ns_string};

use crate::status_item::MenuHandler;

/// Build the main menu and install it on the shared application. AppKit
/// retains the menu; the Quit target is the handler retained by the status
/// item controller.
pub fn install(mtm: MainThreadMarker, quit_handler: &Retained<MenuHandler>) {
    let app = NSApplication::sharedApplication(mtm);
    let main = NSMenu::new(mtm);
    // One submenu carries the items, the conventional shape; its title is
    // never drawn for an accessory app.
    let app_item = NSMenuItem::new(mtm);
    let submenu = NSMenu::new(mtm);

    add_item(&submenu, "Copy", sel!(copy:), ns_string!("c"), None);
    add_item(
        &submenu,
        "Select All",
        sel!(selectAll:),
        ns_string!("a"),
        None,
    );
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(
        &submenu,
        "Close Window",
        sel!(performClose:),
        ns_string!("w"),
        None,
    );
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(
        &submenu,
        "Quit clueless",
        sel!(quitApp:),
        ns_string!("q"),
        Some(quit_handler),
    );

    main.addItem(&app_item);
    app_item.setSubmenu(Some(&submenu));
    app.setMainMenu(Some(&main));
}

/// One menu item with the Command modifier; a `None` target routes the
/// action through the responder chain.
fn add_item(
    menu: &NSMenu,
    title: &str,
    action: objc2::runtime::Sel,
    key: &NSString,
    target: Option<&Retained<MenuHandler>>,
) {
    let item = unsafe {
        menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str(title), Some(action), key)
    };
    let target: Option<&objc2::runtime::AnyObject> = target.map(|t| &****t);
    // SAFETY: plain setter; AppKit menu targets are weak and the one passed
    // here is retained by the status item controller for the process
    // lifetime.
    unsafe {
        item.setTarget(target);
    }
}
