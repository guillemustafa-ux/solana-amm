/**
 * Demo REAL en devnet: crea 2 mints, inicializa el pool (fee 0.30%), agrega
 * liquidez, hace un swap A->B y retira parte de la liquidez. Imprime las firmas
 * de cada transacción (evidencia citable en el README).
 *
 * Uso:  npm run demo
 * Requiere: programa deployado en devnet (Anchor.toml [programs.devnet]) y
 * ~0.05 SOL en la wallet del provider para renta/fees de la demo.
 */
import * as anchor from "@coral-xyz/anchor";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { PublicKey, SystemProgram, SYSVAR_RENT_PUBKEY } from "@solana/web3.js";
import idl from "../target/idl/amm.json";

const FEE_BPS = 30;

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new anchor.Program(idl as anchor.Idl, provider);
  const payer = (provider.wallet as anchor.Wallet).payer;

  console.log("program:", program.programId.toBase58());
  console.log("wallet :", payer.publicKey.toBase58());

  // 1) Dos mints demo (6 decimales), en orden canónico.
  const m1 = await createMint(provider.connection, payer, payer.publicKey, null, 6);
  const m2 = await createMint(provider.connection, payer, payer.publicKey, null, 6);
  const [mintA, mintB] =
    Buffer.compare(m1.toBuffer(), m2.toBuffer()) < 0 ? [m1, m2] : [m2, m1];
  console.log("mint_a :", mintA.toBase58());
  console.log("mint_b :", mintB.toBase58());

  const [pool] = PublicKey.findProgramAddressSync(
    [Buffer.from("pool"), mintA.toBuffer(), mintB.toBuffer()],
    program.programId
  );
  const [lpMint] = PublicKey.findProgramAddressSync(
    [Buffer.from("lp"), pool.toBuffer()],
    program.programId
  );
  const [vaultA] = PublicKey.findProgramAddressSync(
    [Buffer.from("vault_a"), pool.toBuffer()],
    program.programId
  );
  const [vaultB] = PublicKey.findProgramAddressSync(
    [Buffer.from("vault_b"), pool.toBuffer()],
    program.programId
  );
  console.log("pool   :", pool.toBase58());

  // ATAs del usuario + fondos demo.
  const userTokenA = (
    await getOrCreateAssociatedTokenAccount(provider.connection, payer, mintA, payer.publicKey)
  ).address;
  const userTokenB = (
    await getOrCreateAssociatedTokenAccount(provider.connection, payer, mintB, payer.publicKey)
  ).address;
  await mintTo(provider.connection, payer, mintA, userTokenA, payer, 1_000_000_000);
  await mintTo(provider.connection, payer, mintB, userTokenB, payer, 1_000_000_000);

  // 2) initialize_pool
  const sigInit = await program.methods
    .initializePool(FEE_BPS)
    .accounts({
      payer: payer.publicKey,
      mintA,
      mintB,
      pool,
      lpMint,
      vaultA,
      vaultB,
      tokenProgram: TOKEN_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      rent: SYSVAR_RENT_PUBKEY,
    })
    .rpc();
  console.log("initialize_pool  sig:", sigInit);

  const userLp = (
    await getOrCreateAssociatedTokenAccount(provider.connection, payer, lpMint, payer.publicKey)
  ).address;

  // 3) add_liquidity: 100 A + 25 B  ->  LP = sqrt(100e6*25e6) = 50e6
  const sigAdd = await program.methods
    .addLiquidity(new anchor.BN(100_000_000), new anchor.BN(25_000_000), new anchor.BN(0))
    .accounts({
      user: payer.publicKey,
      mintA,
      mintB,
      pool,
      lpMint,
      vaultA,
      vaultB,
      userTokenA,
      userTokenB,
      userLp,
      tokenProgram: TOKEN_PROGRAM_ID,
    })
    .rpc();
  console.log("add_liquidity    sig:", sigAdd);
  console.log("  LP en wallet:", (await getAccount(provider.connection, userLp)).amount.toString());

  // 4) swap 10 A -> B (min_out calculado con la misma fórmula de la curva)
  const va = (await getAccount(provider.connection, vaultA)).amount;
  const vb = (await getAccount(provider.connection, vaultB)).amount;
  const amountIn = 10_000_000n;
  const inAfterFee = (amountIn * (10_000n - BigInt(FEE_BPS))) / 10_000n;
  const expectedOut = (vb * inAfterFee) / (va + inAfterFee);
  const sigSwap = await program.methods
    .swap(new anchor.BN(amountIn.toString()), new anchor.BN(expectedOut.toString()), true)
    .accounts({
      user: payer.publicKey,
      mintA,
      mintB,
      pool,
      vaultA,
      vaultB,
      userTokenA,
      userTokenB,
      tokenProgram: TOKEN_PROGRAM_ID,
    })
    .rpc();
  console.log("swap A->B        sig:", sigSwap);
  console.log("  esperado:", expectedOut.toString(), "de token_b por", amountIn.toString(), "de token_a");
  console.log("  reservas:", {
    vault_a: (await getAccount(provider.connection, vaultA)).amount.toString(),
    vault_b: (await getAccount(provider.connection, vaultB)).amount.toString(),
  });

  // 5) remove_liquidity: la mitad de las cuotas
  const lpBal = (await getAccount(provider.connection, userLp)).amount;
  const sigRemove = await program.methods
    .removeLiquidity(new anchor.BN((lpBal / 2n).toString()), new anchor.BN(0), new anchor.BN(0))
    .accounts({
      user: payer.publicKey,
      mintA,
      mintB,
      pool,
      lpMint,
      vaultA,
      vaultB,
      userTokenA,
      userTokenB,
      userLp,
      tokenProgram: TOKEN_PROGRAM_ID,
    })
    .rpc();
  console.log("remove_liquidity sig:", sigRemove);
  console.log("  LP restante:", (await getAccount(provider.connection, userLp)).amount.toString());
  console.log("  balances usuario:", {
    token_a: (await getAccount(provider.connection, userTokenA)).amount.toString(),
    token_b: (await getAccount(provider.connection, userTokenB)).amount.toString(),
  });

  console.log("\nDemo OK — firmas citables arriba.");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
