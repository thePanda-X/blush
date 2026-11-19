// Builds the web app into dist/, which cargo embeds into the blush binary
// (see ../build.rs). File names are stable (no hashes): the server serves
// them with ETags instead.
import tailwind from "bun-plugin-tailwind";
import { readFileSync, rmSync, watch, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const outdir = fileURLToPath(new URL("./dist", import.meta.url));

async function build() {
  const t = performance.now();
  rmSync(outdir, { recursive: true, force: true });
  const res = await Bun.build({
    entrypoints: ["./src/index.html"],
    outdir,
    minify: true,
    target: "browser",
    sourcemap: "none",
    naming: { entry: "[name].[ext]", chunk: "[name].[ext]", asset: "[name].[ext]" },
    plugins: [tailwind],
    define: { "process.env.NODE_ENV": '"production"' },
  });
  if (!res.success) {
    for (const l of res.logs) console.error(l);
    return false;
  }
  for (const o of res.outputs) {
    const name = o.path.split(/[\\/]/).pop();
    // Precompressed copies; the server sends these to browsers that accept gzip.
    const gz = Bun.gzipSync(readFileSync(o.path), { level: 9 });
    writeFileSync(`${o.path}.gz`, gz);
    console.log(`  ${name}  ${(o.size / 1024).toFixed(1)} KB  (gzip ${(gz.length / 1024).toFixed(1)} KB)`);
  }
  console.log(`built in ${(performance.now() - t).toFixed(0)}ms`);
  return true;
}

const ok = await build();
if (process.argv.includes("--watch")) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  watch("./src", { recursive: true }, () => {
    clearTimeout(timer);
    timer = setTimeout(build, 80);
  });
  console.log("watching src/ …");
} else if (!ok) {
  process.exit(1);
}
