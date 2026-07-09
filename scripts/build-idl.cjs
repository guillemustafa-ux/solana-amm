/**
 * Genera `target/idl/amm.json` en Windows nativo, donde `anchor idl build`
 * falla con "EOF while parsing a value at line 1 column 0".
 *
 * CAUSA: `anchor idl build` corre el harness de test que emite el IDL por
 * fragmentos (program / accounts / types / event / errors / address). Como
 * `cargo test` corre en PARALELO, los fragmentos se interleavean en stdout y el
 * parser de anchor recibe JSON corrupto. En Windows el timing lo dispara siempre.
 *
 * WORKAROUND (2 pasos):
 *   1) Capturar los fragmentos SINGLE-THREADED (sin interleaving):
 *        cargo test -p amm --features idl-build -- --nocapture --test-threads=1 \
 *          > idl-clean.txt 2>/dev/null
 *   2) Ensamblar:
 *        node scripts/build-idl.cjs idl-clean.txt
 *
 * El ensamblado replica lo que hace anchor: junta el fragmento `program`
 * (instructions/accounts/types) con los `event`, los `errors` y el `address`, y
 * stripea el prefijo de módulo (`amm::Pool` -> `Pool`).
 */
const fs = require("fs");
const path = require("path");

const inputFile = process.argv[2] || "idl-clean.txt";
const raw = fs.readFileSync(inputFile, "utf8").split(/\r?\n/);

function frags(kind) {
  const begin = `--- IDL begin ${kind} ---`;
  const end = `--- IDL end ${kind} ---`;
  const out = [];
  let cur = null;
  for (const l of raw) {
    if (l.includes(begin)) { cur = []; continue; }
    if (cur && l.includes(end)) { out.push(cur.join("\n")); cur = null; continue; }
    if (cur) cur.push(l);
  }
  return out.map((s) => JSON.parse(s));
}

// address desde Anchor.toml ([programs.devnet] amm = "...").
const anchorToml = fs.readFileSync("Anchor.toml", "utf8");
const address = anchorToml.match(/amm\s*=\s*"([^"]+)"/)[1];

const program = frags("program")[0];
const errors = frags("errors")[0] || [];
const events = frags("event"); // { event:{...}, types:[...] }

// stripea "modulo::Foo" -> "Foo" en cualquier campo "name".
function strip(node) {
  if (Array.isArray(node)) return node.map(strip);
  if (node && typeof node === "object") {
    const o = {};
    for (const [k, v] of Object.entries(node)) {
      o[k] = k === "name" && typeof v === "string" && v.includes("::") ? v.split("::").pop() : strip(v);
    }
    return o;
  }
  return node;
}

const typesByName = {};
for (const t of program.types || []) typesByName[t.name] = t;
for (const ev of events) for (const t of ev.types || []) typesByName[t.name] = t;

const idl = strip({
  address,
  metadata: {
    name: "amm",
    version: "0.1.0",
    spec: "0.1.0",
    description: "AMM de producto constante (x*y=k) - pieza #11 (Track B Solana)",
  },
  instructions: program.instructions || [],
  accounts: program.accounts || [],
  events: events.map((e) => e.event),
  errors,
  types: Object.values(typesByName),
});

const outPath = path.join("target", "idl", "amm.json");
fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, JSON.stringify(idl, null, 2));
console.log(`IDL escrito en ${outPath}: ${idl.instructions.length} ix, ${idl.accounts.length} accounts, ${idl.events.length} events, ${idl.errors.length} errors`);
