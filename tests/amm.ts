/**
 * Suite de tests del AMM (make/add/remove/swap + reverts).
 *
 * NOTA WINDOWS: `solana-test-validator` no arranca en Windows nativo (genesis
 * "Acceso denegado"), así que esta suite queda escrita pero NO corrió local.
 * La evidencia de funcionamiento es el deploy + la demo real en devnet
 * (scripts/live-demo.ts), documentada en el README.
 */
import * as anchor from "@coral-xyz/anchor";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { assert } from "chai";
import idl from "../target/idl/amm.json";

describe("amm", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new anchor.Program(idl as anchor.Idl, provider);
  const payer = (provider.wallet as anchor.Wallet).payer;

  const FEE_BPS = 30; // 0.30%, el clásico de Uniswap V2

  let mintA: PublicKey;
  let mintB: PublicKey;
  let pool: PublicKey;
  let lpMint: PublicKey;
  let vaultA: PublicKey;
  let vaultB: PublicKey;
  let userTokenA: PublicKey;
  let userTokenB: PublicKey;
  let userLp: PublicKey;

  const pdas = (a: PublicKey, b: PublicKey) => {
    const [poolPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("pool"), a.toBuffer(), b.toBuffer()],
      program.programId
    );
    const [lp] = PublicKey.findProgramAddressSync(
      [Buffer.from("lp"), poolPda.toBuffer()],
      program.programId
    );
    const [va] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault_a"), poolPda.toBuffer()],
      program.programId
    );
    const [vb] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault_b"), poolPda.toBuffer()],
      program.programId
    );
    return { poolPda, lp, va, vb };
  };

  before(async () => {
    // Dos mints ordenados canónicamente (mint_a < mint_b).
    const m1 = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    const m2 = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    [mintA, mintB] =
      Buffer.compare(m1.toBuffer(), m2.toBuffer()) < 0 ? [m1, m2] : [m2, m1];

    ({ poolPda: pool, lp: lpMint, va: vaultA, vb: vaultB } = pdas(mintA, mintB));

    userTokenA = (
      await getOrCreateAssociatedTokenAccount(provider.connection, payer, mintA, payer.publicKey)
    ).address;
    userTokenB = (
      await getOrCreateAssociatedTokenAccount(provider.connection, payer, mintB, payer.publicKey)
    ).address;
    await mintTo(provider.connection, payer, mintA, userTokenA, payer, 1_000_000_000);
    await mintTo(provider.connection, payer, mintB, userTokenB, payer, 1_000_000_000);
  });

  it("inicializa el pool con fee válido", async () => {
    await program.methods
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

    const state: any = await (program.account as any).pool.fetch(pool);
    assert.equal(state.feeBps, FEE_BPS);
    assert.equal(state.mintA.toBase58(), mintA.toBase58());
    assert.equal(state.mintB.toBase58(), mintB.toBase58());
  });

  it("rechaza mints en orden no canónico", async () => {
    const { poolPda } = pdas(mintB, mintA);
    try {
      await program.methods
        .initializePool(FEE_BPS)
        .accounts({
          payer: payer.publicKey,
          mintA: mintB,
          mintB: mintA,
          pool: poolPda,
          lpMint,
          vaultA,
          vaultB,
          tokenProgram: TOKEN_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .rpc();
      assert.fail("debería haber revertido");
    } catch (e: any) {
      assert.include(String(e), "WrongMintOrder");
    }
  });

  it("primer add_liquidity acuña sqrt(a*b) de LP", async () => {
    userLp = (
      await getOrCreateAssociatedTokenAccount(provider.connection, payer, lpMint, payer.publicKey)
    ).address;

    const a = 100_000_000; // 100 tokens
    const b = 25_000_000; // 25 tokens
    await program.methods
      .addLiquidity(new anchor.BN(a), new anchor.BN(b), new anchor.BN(0))
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

    const lp = await getAccount(provider.connection, userLp);
    // sqrt(100e6 * 25e6) = 50e6
    assert.equal(lp.amount.toString(), "50000000");
  });

  it("swap A->B respeta x*y=k y cobra el fee", async () => {
    const va0 = (await getAccount(provider.connection, vaultA)).amount;
    const vb0 = (await getAccount(provider.connection, vaultB)).amount;
    const userB0 = (await getAccount(provider.connection, userTokenB)).amount;

    const amountIn = 10_000_000n;
    const inAfterFee = (amountIn * (10_000n - BigInt(FEE_BPS))) / 10_000n;
    const expectedOut = (vb0 * inAfterFee) / (va0 + inAfterFee);

    await program.methods
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

    const userB1 = (await getAccount(provider.connection, userTokenB)).amount;
    assert.equal((userB1 - userB0).toString(), expectedOut.toString());

    // k no baja (el fee lo sube).
    const va1 = (await getAccount(provider.connection, vaultA)).amount;
    const vb1 = (await getAccount(provider.connection, vaultB)).amount;
    assert.isTrue(va1 * vb1 >= va0 * vb0);
  });

  it("swap con min_amount_out imposible revierte (slippage)", async () => {
    try {
      await program.methods
        .swap(new anchor.BN(1_000_000), new anchor.BN("999999999999"), true)
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
      assert.fail("debería haber revertido");
    } catch (e: any) {
      assert.include(String(e), "SlippageExceeded");
    }
  });

  it("remove_liquidity devuelve pro-rata y quema las cuotas", async () => {
    const lp0 = (await getAccount(provider.connection, userLp)).amount;
    const va0 = (await getAccount(provider.connection, vaultA)).amount;
    const vb0 = (await getAccount(provider.connection, vaultB)).amount;
    const supply = lp0; // único LP

    const burn = lp0 / 2n;
    const expA = (burn * va0) / supply;
    const expB = (burn * vb0) / supply;

    const userA0 = (await getAccount(provider.connection, userTokenA)).amount;
    const userB0 = (await getAccount(provider.connection, userTokenB)).amount;

    await program.methods
      .removeLiquidity(
        new anchor.BN(burn.toString()),
        new anchor.BN(expA.toString()),
        new anchor.BN(expB.toString())
      )
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

    const userA1 = (await getAccount(provider.connection, userTokenA)).amount;
    const userB1 = (await getAccount(provider.connection, userTokenB)).amount;
    const lp1 = (await getAccount(provider.connection, userLp)).amount;

    assert.equal((userA1 - userA0).toString(), expA.toString());
    assert.equal((userB1 - userB0).toString(), expB.toString());
    assert.equal((lp0 - lp1).toString(), burn.toString());
  });

  it("rechaza fee mayor al máximo", async () => {
    const m3 = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    const m4 = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    const [a, b] = Buffer.compare(m3.toBuffer(), m4.toBuffer()) < 0 ? [m3, m4] : [m4, m3];
    const { poolPda, lp, va, vb } = pdas(a, b);
    try {
      await program.methods
        .initializePool(5_000)
        .accounts({
          payer: payer.publicKey,
          mintA: a,
          mintB: b,
          pool: poolPda,
          lpMint: lp,
          vaultA: va,
          vaultB: vb,
          tokenProgram: TOKEN_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .rpc();
      assert.fail("debería haber revertido");
    } catch (e: any) {
      assert.include(String(e), "FeeTooHigh");
    }
  });
});
