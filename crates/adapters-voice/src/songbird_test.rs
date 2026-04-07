use std::time::Duration;
use songbird::{
    Event, EventContext, EventHandler, TrackEvent,
    tracks::{PlayMode, TrackHandle},
    create_player
};
use async_trait::async_trait;

struct DummyHandler;
#[async_trait]
impl EventHandler for DummyHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(ts) = ctx {
            println!("Got event for track! {:?}", ts.first().map(|t| &t.0.playing));
        }
        None
    }
}

#[tokio::main]
async fn main() {
    let (mut track, handle) = create_player(songbird::input::Input::from(songbird::input::core::io::Dummy::new(vec![])));
    handle.add_event(Event::Track(TrackEvent::End), DummyHandler).unwrap();
    handle.stop().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
}
