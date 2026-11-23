//! The running text of a window, read from the accessibility bus itself:
//! agent-desktop reports elements, not the text of paragraphs.

use crate::run::Env;
use zbus::blocking::{connection, Connection};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
/// Stands for a child element inside a text.
const EMBEDDED: char = '\u{FFFC}';
// Bits of the state set, as numbered by AT-SPI.
const SHOWING: u32 = 25;
const VISIBLE: u32 = 30;

/// One object on the bus: the connection name of its app and its path.
#[derive(Clone)]
struct Node(String, OwnedObjectPath);

struct Bus(Connection);

impl Bus {
    /// The accessibility bus of the session: its address is asked of the
    /// session bus.
    fn connect(env: &Env) -> Option<Self> {
        let session = connection::Builder::address(env.get("DBUS_SESSION_BUS_ADDRESS")?.as_str())
            .ok()?
            .build()
            .ok()?;
        let reply = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .ok()?;
        let address: String = reply.body().deserialize().ok()?;
        let bus = connection::Builder::address(address.as_str())
            .ok()?
            .build()
            .ok()?;
        Some(Self(bus))
    }

    fn call<T>(
        &self,
        node: &Node,
        interface: &str,
        method: &str,
        args: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) -> Option<T>
    where
        T: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        let reply = self
            .0
            .call_method(
                Some(node.0.as_str()),
                node.1.as_str(),
                Some(interface),
                method,
                args,
            )
            .ok()?;
        reply.body().deserialize().ok()
    }

    fn children(&self, node: &Node) -> Vec<Node> {
        let found: Vec<(String, OwnedObjectPath)> = self
            .call(node, ACCESSIBLE, "GetChildren", &())
            .unwrap_or_default();
        found
            .into_iter()
            .filter(|(_, path)| path.as_str() != "/org/a11y/atspi/null")
            .map(|(name, path)| Node(name, path))
            .collect()
    }

    fn role(&self, node: &Node) -> String {
        self.call(node, ACCESSIBLE, "GetRoleName", &())
            .unwrap_or_default()
    }

    fn has_state(&self, node: &Node, state: u32) -> bool {
        let words: Vec<u32> = self
            .call(node, ACCESSIBLE, "GetState", &())
            .unwrap_or_default();
        words
            .get((state / 32) as usize)
            .is_some_and(|word| word >> (state % 32) & 1 == 1)
    }

    fn name(&self, node: &Node) -> String {
        let value: Option<OwnedValue> = self.call(
            node,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(ACCESSIBLE, "Name"),
        );
        value
            .and_then(|v| String::try_from(v).ok())
            .unwrap_or_default()
    }

    /// The node's own text, or None when it has no text interface.
    fn text(&self, node: &Node) -> Option<String> {
        self.call(node, "org.a11y.atspi.Text", "GetText", &(0i32, -1i32))
    }

    fn find(&self, node: &Node, wanted: &dyn Fn(&Self, &Node) -> bool, depth: u32) -> Option<Node> {
        if wanted(self, node) {
            return Some(node.clone());
        }
        if depth == 0 {
            return None;
        }
        self.children(node)
            .iter()
            .find_map(|child| self.find(child, wanted, depth - 1))
    }

    fn lines_of(&self, node: &Node) -> Vec<String> {
        // Visible, not showing: what is scrolled out of view belongs to the text.
        if !self.has_state(node, VISIBLE) {
            return Vec::new();
        }
        let role = self.role(node);
        let text = self.text(node);
        let kids = self.children(node);
        if ["entry", "password text", "spin button"].contains(&role.as_str()) {
            let text = text.unwrap_or_default();
            let shown = if role == "password text" {
                "*".repeat(text.chars().count())
            } else {
                text
            };
            return vec![format!("[{shown}]")];
        }
        if let Some(text) = text.filter(|t| !t.replace(EMBEDDED, "").trim().is_empty()) {
            // Running text with its children in place, as one line.
            let mut pieces = text.split(EMBEDDED);
            let mut joined = pieces.next().unwrap_or("").to_string();
            for (index, piece) in pieces.enumerate() {
                if let Some(kid) = kids.get(index) {
                    joined.push_str(&self.lines_of(kid).join(" "));
                }
                joined.push_str(piece);
            }
            return vec![crate::squeeze_spaces(&joined)];
        }
        let below: Vec<String> = kids.iter().flat_map(|kid| self.lines_of(kid)).collect();
        if !below.is_empty() && role == "table row" {
            return vec![below.join(" | ")];
        }
        if !below.is_empty() && role == "list item" {
            return vec![below.join(" ")];
        }
        if below.is_empty() {
            let name = self.name(node);
            if !name.is_empty() {
                return vec![crate::squeeze_spaces(&name)];
            }
        }
        below
    }
}

/// The text of the window with this title as a person reads it, as lines, or
/// None when the accessibility bus cannot be read.
pub fn read_text(env: &Env, title: &str) -> Option<Vec<String>> {
    let bus = Bus::connect(env)?;
    let desktop = Node(
        "org.a11y.atspi.Registry".into(),
        OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").ok()?,
    );
    let is_window = |bus: &Bus, node: &Node| {
        ["frame", "dialog", "window"].contains(&bus.role(node).as_str())
            && bus.name(node) == title
            && bus.has_state(node, SHOWING)
    };
    let window = bus.find(&desktop, &is_window, 2)?;
    // In a browser, the page is what is wanted, not the toolbars around it.
    let is_page =
        |bus: &Bus, node: &Node| bus.role(node) == "document web" && bus.has_state(node, SHOWING);
    let start = bus.find(&window, &is_page, 12).unwrap_or(window);
    Some(
        bus.lines_of(&start)
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect(),
    )
}
