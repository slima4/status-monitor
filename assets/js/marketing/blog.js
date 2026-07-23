import { addCopyButtons } from "./_copy_code.js";

// Progressive enhancement for a blog post: a copy button on every code block.
const article = document.querySelector(".mk-article");
if (article) addCopyButtons(article, "blog");
