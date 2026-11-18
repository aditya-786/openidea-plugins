//! Go to Definition from a Feign client method, narrowed by the service identity.
use super::*;

const MAX_TARGETS: usize = 256;

pub(super) fn definitions(facts: &JavaFacts, at: Position, java: &Index<JavaFacts>, config: &Index<StreamConfig>) -> Vec<Location> {
    let identities = services::Identities::new(config);
    let mut targets = Vec::new();
    for client in facts.clients.iter().filter(|c| contains(c.range, at)) {
        for (path, file) in java.iter().filter(|(p, _)| identities.allows(client, p)) {
            for server in file.servers.iter().filter(|s| calls(client, s)) {
                let location = Location { path: path.into(), range: server.range };
                if !targets.contains(&location) {
                    targets.push(location);
                    if targets.len() == MAX_TARGETS {
                        return targets;
                    }
                }
            }
        }
    }
    targets
}
