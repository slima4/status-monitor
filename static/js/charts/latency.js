import { wireChartElements } from "./_init.js";
import { initLatencyChart } from "./_latency_core.js";

wireChartElements("#latency-chart[data-endpoint]", el => initLatencyChart(el, el.dataset.endpoint));
