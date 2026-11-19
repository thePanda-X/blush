import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import c from "highlight.js/lib/languages/c";
import cpp from "highlight.js/lib/languages/cpp";
import csharp from "highlight.js/lib/languages/csharp";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import kotlin from "highlight.js/lib/languages/kotlin";
import markdown from "highlight.js/lib/languages/markdown";
import php from "highlight.js/lib/languages/php";
import powershell from "highlight.js/lib/languages/powershell";
import python from "highlight.js/lib/languages/python";
import ruby from "highlight.js/lib/languages/ruby";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import swift from "highlight.js/lib/languages/swift";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";
import { Marked, type Tokens } from "marked";

for (const [name, lang] of Object.entries({
  bash, c, cpp, csharp, css, diff, go, ini, java, javascript, json, kotlin, markdown, php, powershell, python, ruby, rust, sql,
  swift, typescript, xml, yaml,
})) {
  hljs.registerLanguage(name, lang);
}
hljs.registerAliases(["sh", "shell", "zsh", "console"], { languageName: "bash" });
hljs.registerAliases(["toml"], { languageName: "ini" });
hljs.registerAliases(["ts", "tsx"], { languageName: "typescript" });
hljs.registerAliases(["js", "jsx", "mjs"], { languageName: "javascript" });
hljs.registerAliases(["py"], { languageName: "python" });
hljs.registerAliases(["rs"], { languageName: "rust" });
hljs.registerAliases(["html", "svg"], { languageName: "xml" });
hljs.registerAliases(["yml"], { languageName: "yaml" });
hljs.registerAliases(["ps1", "pwsh"], { languageName: "powershell" });

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const marked = new Marked({
  gfm: true,
  breaks: false,
  renderer: {
    // Raw HTML in a reply is shown as text, never rendered.
    html(t: Tokens.HTML | Tokens.Tag) {
      return esc(t.text);
    },
    // Task-list boxes as glyphs, like the TUI (inputs don't fit the theme).
    checkbox({ checked }: Tokens.Checkbox) {
      return checked ? '<span class="task done">✓</span> ' : '<span class="task">☐</span> ';
    },
    code(t: Tokens.Code) {
      const lang = (t.lang ?? "").trim().split(/\s+/)[0];
      let body: string;
      try {
        body = lang && hljs.getLanguage(lang) ? hljs.highlight(t.text, { language: lang }).value : esc(t.text);
      } catch {
        body = esc(t.text);
      }
      return `<pre${lang ? ` data-lang="${esc(lang)}"` : ""}><code>${body}</code></pre>`;
    },
  },
});

DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

const cache = new Map<string, string>();

export function renderMarkdown(src: string): string {
  const hit = cache.get(src);
  if (hit !== undefined) return hit;
  const html = DOMPurify.sanitize(marked.parse(src, { async: false }) as string, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["style", "form", "input", "textarea", "select", "button", "iframe"],
    ALLOWED_URI_REGEXP: /^(?:https?|mailto|file):/i,
  });
  const out = html;
  if (cache.size > 400) cache.clear();
  cache.set(src, out);
  return out;
}

export function highlight(code: string, lang?: string): string {
  try {
    if (lang && hljs.getLanguage(lang)) return hljs.highlight(code, { language: lang }).value;
  } catch {
    /* fall through */
  }
  return esc(code);
}

export function langOf(path: string): string | undefined {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    rs: "rust", ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", mjs: "javascript",
    py: "python", go: "go", rb: "ruby", java: "java", kt: "kotlin", swift: "swift", c: "c", h: "c",
    cpp: "cpp", cc: "cpp", hpp: "cpp", cs: "csharp", php: "php", sh: "bash", bash: "bash", zsh: "bash",
    ps1: "powershell", json: "json", toml: "ini", ini: "ini", yaml: "yaml", yml: "yaml", md: "markdown",
    html: "xml", xml: "xml", css: "css", scss: "scss", sql: "sql", lua: "lua", r: "r", makefile: "makefile",
  };
  return map[ext];
}
