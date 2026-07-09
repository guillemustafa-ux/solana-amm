# solana-amm — AMM de producto constante (x·y=k)

Pieza **#11** del portfolio (Track B Solana). Un market maker automático estilo
Uniswap V2 / Orca escrito en **Anchor**: pools por par de tokens SPL, provisión
de liquidez con LP tokens, y swaps contra la curva `x·y=k` con fee configurable.

Completa la escalera DeFi del track Solana:

| # | Pieza | Primitiva |
|---|-------|-----------|
| 7 | [solana-access-pass](https://github.com/guillemustafa-ux/solana-access-pass) | Suscripción por tiempo (unilateral) |
| 10 | [solana-escrow](https://github.com/guillemustafa-ux/solana-escrow) | Swap atómico OTC (bilateral, 1 contraparte) |
| **11** | **solana-amm** | **Mercado continuo sin contraparte (la curva es el precio)** |

## Instrucciones del programa

- **`initialize_pool(fee_bps)`** — crea el pool para `(mint_a, mint_b)` con
  orden canónico `mint_a < mint_b` (a lo sumo un pool por par, sin factory),
  el LP mint y las dos bóvedas. Fee máximo 10% (1000 bps).
- **`add_liquidity(amount_a, amount_b, min_lp)`** — deposita en las bóvedas y
  acuña LP: el primer depósito recibe `√(a·b)` (con piso `MINIMUM_LIQUIDITY`);
  los siguientes, `min` de las dos proporciones (aportar desbalanceado nunca
  acuña de más).
- **`remove_liquidity(lp_amount, min_a, min_b)`** — quema cuotas y retira
  pro-rata de ambas reservas.
- **`swap(amount_in, min_amount_out, a_to_b)`** — `out = R_out·Δ/(R_in+Δ)` con
  `Δ = amount_in` neto de fee. El fee queda en la reserva de entrada (lo cobran
  los LPs al retirar). Todo el redondeo es hacia abajo, a favor del pool: el
  invariante `k` nunca baja.

## EVM ↔ Solana (Uniswap V2 vs este AMM)

| Concepto | Uniswap V2 (EVM) | Este AMM (Solana) |
|---|---|---|
| Identidad del pool | Contrato `Pair` clonado por la `Factory` | **PDA** `["pool", mint_a, mint_b]` — el orden canónico reemplaza a la factory |
| Reservas | Variables de estado `reserve0/reserve1` (+ `sync`) | Los **token accounts bóveda son la fuente de verdad**; se leen al inicio de cada ix, no hay estado espejado que desincronizar |
| LP token | El `Pair` *es* un ERC-20 | Mint SPL separado con `authority = pool PDA` |
| Custodia | El contrato posee los tokens | Bóvedas con `authority = pool PDA`; el programa firma por seeds (`invoke_signed`) |
| Autorización | `msg.sender` + `transferFrom` (approve previo) | El usuario **firma la misma tx**; sus transfers salen de sus ATAs sin approve |
| Anti-manipulación 1er LP | `MINIMUM_LIQUIDITY` quemado a `address(0)` | Piso `MINIMUM_LIQUIDITY` en el primer depósito (trade-off documentado abajo) |

## Trade-offs asumidos (demo-grade, honestos)

- **Sin quema de las primeras 1000 cuotas**: Uniswap V2 las quema a `address(0)`;
  acá se exige que el primer depósito produzca ≥1000 cuotas pero no se queman
  (hacerlo requeriría un token account muerto). Suficiente contra montos polvo,
  no contra un primer LP hostil con capital — para un AMM de producción, quemar.
- **Sin oráculo TWAP** ni `skim/sync`: las reservas se leen de las bóvedas.
- **Fee simple** sobre el input, sin protocol fee.
- **Tests locales sin correr**: `solana-test-validator` no arranca en Windows
  nativo (genesis "Acceso denegado"). La suite `tests/amm.ts` queda escrita; la
  evidencia de funcionamiento es el **deploy + demo real en devnet** de abajo.

## Evidencia en devnet

| Qué | Valor |
|---|---|
| Program ID | `ExDFnNBfP6E14ZitQbXQdxqTQvcBbxEcjyVJJ8b8J6j3` |
| Deploy | _(pendiente)_ |
| `initialize_pool` | _(pendiente)_ |
| `add_liquidity` | _(pendiente)_ |
| `swap` A→B | _(pendiente)_ |
| `remove_liquidity` | _(pendiente)_ |

Explorer: https://explorer.solana.com/address/ExDFnNBfP6E14ZitQbXQdxqTQvcBbxEcjyVJJ8b8J6j3?cluster=devnet

## Build en Windows nativo (gotchas)

```bash
# 1) Compilar (NO confiar en exit code piped; verificar que existe el .so)
cargo-build-sbf > build.log 2>&1
ls target/deploy/amm.so

# 2) IDL — `anchor idl build` falla en Windows ("EOF while parsing"):
#    los fragmentos del IDL se interleavean porque cargo test corre en paralelo.
#    Capturar single-threaded y ensamblar:
cargo test -p amm --features idl-build -- --nocapture --test-threads=1 > idl-clean.txt 2>/dev/null
node scripts/build-idl.cjs idl-clean.txt

# 3) Deploy
solana program deploy target/deploy/amm.so --program-id target/deploy/amm-keypair.json --url devnet

# 4) Demo real en devnet (mints + pool + add + swap + remove, imprime firmas)
npm run demo
```

> El keypair del programa (`target/deploy/amm-keypair.json`) es la **upgrade
> authority**: está gitignoreado y nunca se commitea.

## Estructura

```
programs/amm/src/lib.rs   # el programa (4 instrucciones, eventos, errores)
tests/amm.ts              # suite ts-mocha (happy paths + reverts)
scripts/live-demo.ts      # demo end-to-end contra devnet
scripts/build-idl.cjs     # workaround IDL para Windows nativo
```
