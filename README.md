This repo contains code for building a map of bluesky based off who follows who. It also has community detection, which is suprisingly good, as it has found multiple communities I was already aware of. To see the actual map, get the wasm build from the releases tab.

The process was:
1. Scrape all atproto posts across an interval (`collect.rs`)
2. Scrape the followers and followees of those accounts (`bsky.rs`)
3. Create a graph using that data and save it to json (`graph.rs`)
4. Detect communities (`display.rs`)
5. Display and find optimal layout for graph (`display.rs` and `forces.rs`)

No docs, but it's fairly commented.