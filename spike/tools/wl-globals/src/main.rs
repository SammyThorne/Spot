//! Lists the Wayland globals the compositor advertises (read-only; connects, roundtrips, exits).
use wayland_client::{protocol::wl_registry, Connection, Dispatch, QueueHandle};

struct Globals(Vec<(String, u32)>);

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(state: &mut Self, _: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(),
             _: &Connection, _: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { interface, version, .. } = event {
            state.0.push((interface, version));
        }
    }
}

fn main() {
    let conn = Connection::connect_to_env().expect("WAYLAND_DISPLAY not reachable");
    let mut queue = conn.new_event_queue();
    conn.display().get_registry(&queue.handle(), ());
    let mut g = Globals(Vec::new());
    queue.roundtrip(&mut g).expect("roundtrip");
    g.0.sort();
    for (iface, v) in g.0 {
        println!("{iface} v{v}");
    }
}
