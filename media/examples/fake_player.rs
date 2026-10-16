//! A fake MPRIS media player, for tests that must not touch a real one.
//!
//! It registers players that do not exist, so it must never be started against a bus that
//! somebody uses: it refuses to run unless `FAKE_PLAYER_PRIVATE_BUS=1` is set, and that is to be
//! set only together with the address of a bus made for the occasion.
//!
//! `fake_player <bus-name>...` connects to the bus in `DBUS_SESSION_BUS_ADDRESS`, exports the two
//! MPRIS interfaces on `/org/mpris/MediaPlayer2` and takes every `<bus-name>` (a test that needs
//! many players at once lists many names). It prints one line per
//! event on stdout and obeys one command per line on stdin; end of input quits it, so a fake never
//! outlives the test that started it.
//!
//! Output: `ready` once the name is owned, `call <Method>` for every method call (before the
//! reply), `read <Interface>.<Property>` for every property read, `pong <token>` for every
//! `ping <token>`.
//!
//! Commands:
//! - `identity <text>`: the `Identity` property (not signalled, as MPRIS has no signal for it).
//! - `status <Playing|Paused|Stopped>`, `can <play|pause|next|previous|control> <true|false>`,
//!   `art <url>` (no argument: no cover) and `title <text>` change a Player property and emit
//!   `PropertiesChanged`.
//! - `hang`: from now on method calls are accepted and never answered.
//! - `vanish <Property>`: the next time that property is read, the bus names are given up just
//!   before the answer goes out, as a player that quits at that moment (the answer still arrives).
//! - `die <Property>`: the next time that property is read, the process ends at once, so the read
//!   (a `Get` or a `GetAll`) is left unanswered.
//! - `ping <token>`, `quit`.
use glib::Variant;
use glib::prelude::ToVariant;
use std::{
    cell::RefCell,
    collections::HashMap,
    io::{BufRead, Write},
    rc::Rc,
};

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

const XML: &str = r#"<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Play"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Next"/>
    <method name="Previous"/>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>"#;

const CAPABILITIES: [(&str, &str); 5] = [
    ("play", "CanPlay"),
    ("pause", "CanPause"),
    ("next", "CanGoNext"),
    ("previous", "CanGoPrevious"),
    ("control", "CanControl"),
];

struct State {
    identity: String,
    status: String,
    can: HashMap<&'static str, bool>,
    art_url: Option<String>,
    title: Option<String>,
    hang: bool,
    /// The property whose next read makes the player give up its bus names.
    vanish: Option<String>,
    /// The property whose next read makes the player end.
    die: Option<String>,
    /// Calls accepted and never answered, kept alive so that they stay pending.
    pending: Vec<gio::DBusMethodInvocation>,
}

impl State {
    fn metadata(&self) -> Variant {
        let mut map: HashMap<String, Variant> = HashMap::new();
        map.insert(
            "mpris:trackid".into(),
            glib::variant::ObjectPath::try_from("/track/1".to_owned())
                .unwrap()
                .to_variant(),
        );
        if let Some(url) = &self.art_url {
            map.insert("mpris:artUrl".into(), url.to_variant());
        }
        if let Some(title) = &self.title {
            map.insert("xesam:title".into(), title.to_variant());
        }
        map.to_variant()
    }

    fn property(&self, interface: &str, name: &str) -> Variant {
        match (interface, name) {
            (ROOT, "Identity") => self.identity.to_variant(),
            (ROOT, "CanQuit") => false.to_variant(),
            (PLAYER, "PlaybackStatus") => self.status.to_variant(),
            (PLAYER, "Metadata") => self.metadata(),
            (PLAYER, other) => {
                let key = CAPABILITIES.iter().find(|(_, p)| *p == other).map(|c| c.0);
                self.can[key.expect("known property")].to_variant()
            }
            _ => panic!("unknown property {interface}.{name}"),
        }
    }
}

struct Fake {
    connection: gio::DBusConnection,
    names: Vec<String>,
    state: RefCell<State>,
    quit: glib::MainLoop,
}

impl Fake {
    fn emit_changed(&self, names: &[&str]) {
        let state = self.state.borrow();
        let changed: HashMap<String, Variant> = names
            .iter()
            .map(|n| ((*n).to_owned(), state.property(PLAYER, n)))
            .collect();
        let invalidated: Vec<String> = Vec::new();
        let _ = self.connection.emit_signal(
            None::<&str>,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some(&(PLAYER, changed, invalidated).to_variant()),
        );
    }

    fn command(&self, line: &str) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "identity" => self.state.borrow_mut().identity = rest.into(),
            "status" => {
                self.state.borrow_mut().status = rest.into();
                self.emit_changed(&["PlaybackStatus"]);
            }
            "can" => {
                let (what, value) = rest.split_once(' ').expect("can <what> <bool>");
                let (key, property) = CAPABILITIES
                    .iter()
                    .find(|(k, _)| *k == what)
                    .expect("known capability");
                self.state.borrow_mut().can.insert(key, value == "true");
                self.emit_changed(&[property]);
            }
            "art" => {
                self.state.borrow_mut().art_url = (!rest.is_empty()).then(|| rest.to_owned());
                self.emit_changed(&["Metadata"]);
            }
            "title" => {
                self.state.borrow_mut().title = Some(rest.to_owned());
                self.emit_changed(&["Metadata"]);
            }
            "hang" => self.state.borrow_mut().hang = true,
            "vanish" => self.state.borrow_mut().vanish = Some(rest.to_owned()),
            "die" => self.state.borrow_mut().die = Some(rest.to_owned()),
            "ping" => say(&format!("pong {rest}")),
            "quit" => self.quit.quit(),
            other => panic!("unknown command {other}"),
        }
    }

    /// Ends the player, or gives up its bus names, if `property` is the one that was to make it.
    fn vanish_on_read(&self, property: &str) {
        if self.state.borrow().die.as_deref() == Some(property) {
            std::process::exit(0);
        }
        let vanishing = self.state.borrow_mut().vanish.take_if(|p| p == property);
        if vanishing.is_none() {
            return;
        }
        for name in &self.names {
            let _ = self.connection.call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ReleaseName",
                Some(&(name.as_str(),).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
            );
        }
    }

    fn method_call(&self, interface: &str, method: &str, invocation: gio::DBusMethodInvocation) {
        if interface == PLAYER {
            say(&format!("call {method}"));
        }
        let mut state = self.state.borrow_mut();
        if state.hang {
            state.pending.push(invocation);
        } else {
            invocation.return_value(None);
        }
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

thread_local! {
    static FAKE: RefCell<Option<Rc<Fake>>> = const { RefCell::new(None) };
}

fn main() {
    if std::env::var("FAKE_PLAYER_PRIVATE_BUS").as_deref() != Ok("1") {
        eprintln!("fake_player: set FAKE_PLAYER_PRIVATE_BUS=1, with the address of a private bus");
        std::process::exit(2);
    }
    let names: Vec<String> = std::env::args().skip(1).collect();
    assert!(!names.is_empty(), "bus names of the fake player");
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("bus address");
    let connection = gio::DBusConnection::for_address_sync(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        None::<&gio::Cancellable>,
    )
    .expect("connect to the bus");
    let fake = Rc::new(Fake {
        connection: connection.clone(),
        names: names.clone(),
        state: RefCell::new(State {
            identity: "Fake Player".into(),
            status: "Stopped".into(),
            can: CAPABILITIES.iter().map(|(k, _)| (*k, true)).collect(),
            art_url: None,
            title: None,
            hang: false,
            vanish: None,
            die: None,
            pending: Vec::new(),
        }),
        quit: glib::MainLoop::new(None, false),
    });

    let info = gio::DBusNodeInfo::for_xml(XML).expect("interface XML");
    for interface in [ROOT, PLAYER] {
        let description = info.lookup_interface(interface).expect("interface");
        let (calls, reads) = (Rc::clone(&fake), Rc::clone(&fake));
        connection
            .register_object(PATH, &description)
            .method_call(move |_, _, _, interface, method, _, invocation| {
                calls.method_call(interface.unwrap_or_default(), method, invocation);
            })
            .property(move |_, _, _, interface, property| {
                say(&format!("read {interface}.{property}"));
                reads.vanish_on_read(property);
                reads.state.borrow().property(interface, property)
            })
            .build()
            .expect("register object");
    }
    for name in &names {
        let reply = connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&(name.as_str(), 0u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
            )
            .expect("request the name");
        assert_eq!(reply.child_value(0).get::<u32>(), Some(1), "name is taken");
    }

    FAKE.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&fake)));
    // Own the context from here on, so that the input thread can only queue commands onto it.
    let context = glib::MainContext::default();
    let _owner = context.acquire().expect("main context");
    let queue = context.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            queue.invoke(move || {
                FAKE.with(|slot| slot.borrow().as_ref().expect("fake").command(&line));
            });
        }
        // End of input: the test is over (or gone).
        queue.invoke(|| FAKE.with(|slot| slot.borrow().as_ref().expect("fake").command("quit")));
    });
    say("ready");
    fake.quit.run();
}
