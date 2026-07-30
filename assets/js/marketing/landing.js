import { addCopyButtons } from "./_copy_code.js";

// Progressive enhancement for a landing's code sample, the same button the
// blog and docs get.
const sample = document.querySelector(".mk-codeblock");
if (sample) addCopyButtons(sample.parentElement, "landing");
