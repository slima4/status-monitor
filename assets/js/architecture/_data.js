// Same JSON the server renders from, so the map and the crawlable HTML
// cannot drift apart. esbuild inlines it, so there is no extra request.
import data from "./flows.json";

export const NODES = data.nodes;
export const FLOWS = data.flows;
