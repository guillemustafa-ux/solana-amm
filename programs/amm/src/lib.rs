use anchor_lang::prelude::*;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount, Transfer};

declare_id!("ExDFnNBfP6E14ZitQbXQdxqTQvcBbxEcjyVJJ8b8J6j3");

/// Base de puntos básicos: fee_bps se expresa sobre 10_000.
pub const BPS_DENOMINATOR: u64 = 10_000;
/// Fee máximo permitido al crear un pool (10%).
pub const MAX_FEE_BPS: u16 = 1_000;
/// Liquidez mínima que debe producir el primer depósito (mitiga la
/// manipulación del primer LP con montos polvo; ver README, trade-offs).
pub const MINIMUM_LIQUIDITY: u64 = 1_000;

/// # AMM — market maker de producto constante (x·y=k)
///
/// Pieza #11 del portfolio (Track B Solana). La primitiva DeFi que completa la
/// escalera: después del escrow bilateral (#10, un swap = una contraparte), el
/// AMM elimina la contraparte — el precio lo fija la curva `x·y=k` y cualquiera
/// puede operar contra el pool en todo momento (el modelo de Uniswap V2 / Orca).
///
/// Arquitectura Solana (vs. un Uniswap V2 en EVM):
///   - El **`Pool` (PDA)** derivado de `(mint_a, mint_b)` con orden canónico
///     (`mint_a < mint_b`) garantiza a lo sumo un pool por par, sin factory.
///   - Dos **bóvedas** (token accounts con authority = Pool PDA) custodian las
///     reservas; solo el programa puede moverlas, firmando por seeds.
///   - El **LP mint** (authority = Pool PDA) emite las cuotas de los proveedores
///     de liquidez; equivale al token ERC-20 que el par de Uniswap V2 *es* —
///     acá son cuentas separadas coordinadas por el PDA.
///   - Las reservas se leen de las bóvedas al inicio de cada instrucción (antes
///     de cualquier transferencia), no de un estado espejado: una cuenta menos
///     que desincronizar.
#[program]
pub mod amm {
    use super::*;

    /// Crea el pool para el par `(mint_a, mint_b)` con un fee en puntos básicos
    /// que cobra la curva en cada swap (queda en las reservas, a favor de los LPs).
    pub fn initialize_pool(ctx: Context<InitializePool>, fee_bps: u16) -> Result<()> {
        require!(fee_bps <= MAX_FEE_BPS, AmmError::FeeTooHigh);

        let pool = &mut ctx.accounts.pool;
        pool.mint_a = ctx.accounts.mint_a.key();
        pool.mint_b = ctx.accounts.mint_b.key();
        pool.lp_mint = ctx.accounts.lp_mint.key();
        pool.fee_bps = fee_bps;
        pool.bump = ctx.bumps.pool;
        pool.lp_bump = ctx.bumps.lp_mint;
        pool.vault_a_bump = ctx.bumps.vault_a;
        pool.vault_b_bump = ctx.bumps.vault_b;

        emit!(PoolInitialized {
            pool: pool.key(),
            mint_a: pool.mint_a,
            mint_b: pool.mint_b,
            lp_mint: pool.lp_mint,
            fee_bps,
        });
        Ok(())
    }

    /// Deposita `amount_a` y `amount_b` en las bóvedas y acuña LP tokens.
    ///
    /// - Primer depósito: LP = √(a·b) (media geométrica — el valor inicial de la
    ///   cuota no depende de la escala de ninguno de los dos tokens).
    /// - Depósitos siguientes: LP = min(a·supply/reserva_a, b·supply/reserva_b) —
    ///   el mínimo de las dos proporciones, así aportar desbalanceado nunca
    ///   acuña de más (el excedente queda donado a las reservas).
    pub fn add_liquidity(
        ctx: Context<ModifyLiquidity>,
        amount_a: u64,
        amount_b: u64,
        min_lp: u64,
    ) -> Result<()> {
        require!(amount_a > 0 && amount_b > 0, AmmError::ZeroAmount);

        // Reservas y supply ANTES de mover fondos.
        let reserve_a = ctx.accounts.vault_a.amount;
        let reserve_b = ctx.accounts.vault_b.amount;
        let lp_supply = ctx.accounts.lp_mint.supply;

        let lp_to_mint: u64 = if lp_supply == 0 {
            let lp = isqrt((amount_a as u128) * (amount_b as u128)) as u64;
            require!(lp >= MINIMUM_LIQUIDITY, AmmError::InsufficientInitialLiquidity);
            lp
        } else {
            // Con supply > 0 las reservas son > 0 (no hay burn sin supply).
            let by_a = (amount_a as u128) * (lp_supply as u128) / (reserve_a as u128);
            let by_b = (amount_b as u128) * (lp_supply as u128) / (reserve_b as u128);
            u64::try_from(by_a.min(by_b)).map_err(|_| AmmError::MathOverflow)?
        };
        require!(lp_to_mint > 0, AmmError::InsufficientLiquidityMinted);
        require!(lp_to_mint >= min_lp, AmmError::SlippageExceeded);

        // Depósitos: usuario -> bóvedas (firma el usuario).
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.user_token_a.to_account_info(),
                    to: ctx.accounts.vault_a.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount_a,
        )?;
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.user_token_b.to_account_info(),
                    to: ctx.accounts.vault_b.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount_b,
        )?;

        // Acuña LP al usuario (firma el PDA del pool).
        let mint_a = ctx.accounts.pool.mint_a;
        let mint_b = ctx.accounts.pool.mint_b;
        let bump = ctx.accounts.pool.bump;
        let signer_seeds: &[&[u8]] =
            &[b"pool", mint_a.as_ref(), mint_b.as_ref(), std::slice::from_ref(&bump)];
        token::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                MintTo {
                    mint: ctx.accounts.lp_mint.to_account_info(),
                    to: ctx.accounts.user_lp.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                &[signer_seeds],
            ),
            lp_to_mint,
        )?;

        emit!(LiquidityAdded {
            pool: ctx.accounts.pool.key(),
            user: ctx.accounts.user.key(),
            amount_a,
            amount_b,
            lp_minted: lp_to_mint,
        });
        Ok(())
    }

    /// Quema `lp_amount` y retira la parte proporcional de ambas reservas.
    pub fn remove_liquidity(
        ctx: Context<ModifyLiquidity>,
        lp_amount: u64,
        min_a: u64,
        min_b: u64,
    ) -> Result<()> {
        require!(lp_amount > 0, AmmError::ZeroAmount);

        let reserve_a = ctx.accounts.vault_a.amount;
        let reserve_b = ctx.accounts.vault_b.amount;
        let lp_supply = ctx.accounts.lp_mint.supply;
        require!(lp_supply > 0, AmmError::EmptyPool);

        // Pro-rata, redondeado hacia abajo (a favor del pool).
        let amount_a = u64::try_from((lp_amount as u128) * (reserve_a as u128) / (lp_supply as u128))
            .map_err(|_| AmmError::MathOverflow)?;
        let amount_b = u64::try_from((lp_amount as u128) * (reserve_b as u128) / (lp_supply as u128))
            .map_err(|_| AmmError::MathOverflow)?;
        require!(amount_a >= min_a && amount_b >= min_b, AmmError::SlippageExceeded);

        // Quema las cuotas del usuario (firma el usuario, dueño de su LP account).
        token::burn(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                Burn {
                    mint: ctx.accounts.lp_mint.to_account_info(),
                    from: ctx.accounts.user_lp.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            lp_amount,
        )?;

        // Retiros: bóvedas -> usuario (firma el PDA del pool).
        let mint_a = ctx.accounts.pool.mint_a;
        let mint_b = ctx.accounts.pool.mint_b;
        let bump = ctx.accounts.pool.bump;
        let signer_seeds: &[&[u8]] =
            &[b"pool", mint_a.as_ref(), mint_b.as_ref(), std::slice::from_ref(&bump)];
        if amount_a > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.key(),
                    Transfer {
                        from: ctx.accounts.vault_a.to_account_info(),
                        to: ctx.accounts.user_token_a.to_account_info(),
                        authority: ctx.accounts.pool.to_account_info(),
                    },
                    &[signer_seeds],
                ),
                amount_a,
            )?;
        }
        if amount_b > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.key(),
                    Transfer {
                        from: ctx.accounts.vault_b.to_account_info(),
                        to: ctx.accounts.user_token_b.to_account_info(),
                        authority: ctx.accounts.pool.to_account_info(),
                    },
                    &[signer_seeds],
                ),
                amount_b,
            )?;
        }

        emit!(LiquidityRemoved {
            pool: ctx.accounts.pool.key(),
            user: ctx.accounts.user.key(),
            amount_a,
            amount_b,
            lp_burned: lp_amount,
        });
        Ok(())
    }

    /// Intercambia `amount_in` de un token por el otro según la curva x·y=k.
    ///
    /// `a_to_b = true` entrega token A y recibe token B; `false`, al revés.
    /// El fee se descuenta del input y queda en la reserva de entrada (los LPs
    /// lo cobran al retirar). Fórmula (redondeo hacia abajo, a favor del pool):
    ///   out = reserva_out · in_neto / (reserva_in + in_neto)
    pub fn swap(
        ctx: Context<Swap>,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<()> {
        require!(amount_in > 0, AmmError::ZeroAmount);

        let reserve_a = ctx.accounts.vault_a.amount;
        let reserve_b = ctx.accounts.vault_b.amount;
        require!(reserve_a > 0 && reserve_b > 0, AmmError::EmptyPool);

        let (reserve_in, reserve_out) =
            if a_to_b { (reserve_a, reserve_b) } else { (reserve_b, reserve_a) };

        let fee_bps = ctx.accounts.pool.fee_bps as u128;
        let in_after_fee =
            (amount_in as u128) * (BPS_DENOMINATOR as u128 - fee_bps) / (BPS_DENOMINATOR as u128);
        require!(in_after_fee > 0, AmmError::ZeroAmount);

        let amount_out = u64::try_from(
            (reserve_out as u128) * in_after_fee / ((reserve_in as u128) + in_after_fee),
        )
        .map_err(|_| AmmError::MathOverflow)?;
        require!(amount_out > 0, AmmError::InsufficientOutput);
        require!(amount_out >= min_amount_out, AmmError::SlippageExceeded);
        // La división redondea hacia abajo, así que amount_out < reserve_out
        // siempre: la reserva de salida nunca se vacía del todo.

        let (user_in, user_out, vault_in, vault_out) = if a_to_b {
            (
                ctx.accounts.user_token_a.to_account_info(),
                ctx.accounts.user_token_b.to_account_info(),
                ctx.accounts.vault_a.to_account_info(),
                ctx.accounts.vault_b.to_account_info(),
            )
        } else {
            (
                ctx.accounts.user_token_b.to_account_info(),
                ctx.accounts.user_token_a.to_account_info(),
                ctx.accounts.vault_b.to_account_info(),
                ctx.accounts.vault_a.to_account_info(),
            )
        };

        // Pata 1: usuario -> bóveda de entrada (firma el usuario).
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: user_in,
                    to: vault_in,
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount_in,
        )?;

        // Pata 2: bóveda de salida -> usuario (firma el PDA del pool).
        let mint_a = ctx.accounts.pool.mint_a;
        let mint_b = ctx.accounts.pool.mint_b;
        let bump = ctx.accounts.pool.bump;
        let signer_seeds: &[&[u8]] =
            &[b"pool", mint_a.as_ref(), mint_b.as_ref(), std::slice::from_ref(&bump)];
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: vault_out,
                    to: user_out,
                    authority: ctx.accounts.pool.to_account_info(),
                },
                &[signer_seeds],
            ),
            amount_out,
        )?;

        emit!(Swapped {
            pool: ctx.accounts.pool.key(),
            user: ctx.accounts.user.key(),
            a_to_b,
            amount_in,
            amount_out,
        });
        Ok(())
    }
}

/// Raíz cuadrada entera (método babilónico), redondeada hacia abajo.
fn isqrt(y: u128) -> u128 {
    if y == 0 {
        return 0;
    }
    let mut z = y;
    let mut x = y / 2 + 1;
    while x < z {
        z = x;
        x = (y / x + x) / 2;
    }
    z
}

#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub lp_mint: Pubkey,
    pub fee_bps: u16,
    pub bump: u8,
    pub lp_bump: u8,
    pub vault_a_bump: u8,
    pub vault_b_bump: u8,
}

#[derive(Accounts)]
pub struct InitializePool<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub mint_a: Box<Account<'info, Mint>>,
    pub mint_b: Box<Account<'info, Mint>>,

    /// Orden canónico `mint_a < mint_b`: un único pool posible por par.
    #[account(
        init,
        payer = payer,
        space = 8 + Pool::INIT_SPACE,
        seeds = [b"pool", mint_a.key().as_ref(), mint_b.key().as_ref()],
        bump,
        constraint = mint_a.key() < mint_b.key() @ AmmError::WrongMintOrder,
    )]
    pub pool: Account<'info, Pool>,

    /// Mint de las cuotas LP; solo el PDA del pool puede acuñar.
    #[account(
        init,
        payer = payer,
        seeds = [b"lp", pool.key().as_ref()],
        bump,
        mint::decimals = 6,
        mint::authority = pool,
    )]
    pub lp_mint: Box<Account<'info, Mint>>,

    #[account(
        init,
        payer = payer,
        seeds = [b"vault_a", pool.key().as_ref()],
        bump,
        token::mint = mint_a,
        token::authority = pool,
    )]
    pub vault_a: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = payer,
        seeds = [b"vault_b", pool.key().as_ref()],
        bump,
        token::mint = mint_b,
        token::authority = pool,
    )]
    pub vault_b: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

/// Cuentas compartidas por `add_liquidity` y `remove_liquidity`.
#[derive(Accounts)]
pub struct ModifyLiquidity<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    pub mint_a: Box<Account<'info, Mint>>,
    pub mint_b: Box<Account<'info, Mint>>,

    #[account(
        seeds = [b"pool", mint_a.key().as_ref(), mint_b.key().as_ref()],
        bump = pool.bump,
        has_one = mint_a @ AmmError::MintMismatch,
        has_one = mint_b @ AmmError::MintMismatch,
        has_one = lp_mint @ AmmError::MintMismatch,
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        seeds = [b"lp", pool.key().as_ref()],
        bump = pool.lp_bump,
    )]
    pub lp_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        seeds = [b"vault_a", pool.key().as_ref()],
        bump = pool.vault_a_bump,
    )]
    pub vault_a: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [b"vault_b", pool.key().as_ref()],
        bump = pool.vault_b_bump,
    )]
    pub vault_b: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = user_token_a.mint == pool.mint_a @ AmmError::MintMismatch,
        constraint = user_token_a.owner == user.key() @ AmmError::OwnerMismatch,
    )]
    pub user_token_a: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = user_token_b.mint == pool.mint_b @ AmmError::MintMismatch,
        constraint = user_token_b.owner == user.key() @ AmmError::OwnerMismatch,
    )]
    pub user_token_b: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = user_lp.mint == pool.lp_mint @ AmmError::MintMismatch,
        constraint = user_lp.owner == user.key() @ AmmError::OwnerMismatch,
    )]
    pub user_lp: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Swap<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    pub mint_a: Box<Account<'info, Mint>>,
    pub mint_b: Box<Account<'info, Mint>>,

    #[account(
        seeds = [b"pool", mint_a.key().as_ref(), mint_b.key().as_ref()],
        bump = pool.bump,
        has_one = mint_a @ AmmError::MintMismatch,
        has_one = mint_b @ AmmError::MintMismatch,
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        seeds = [b"vault_a", pool.key().as_ref()],
        bump = pool.vault_a_bump,
    )]
    pub vault_a: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [b"vault_b", pool.key().as_ref()],
        bump = pool.vault_b_bump,
    )]
    pub vault_b: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = user_token_a.mint == pool.mint_a @ AmmError::MintMismatch,
        constraint = user_token_a.owner == user.key() @ AmmError::OwnerMismatch,
    )]
    pub user_token_a: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = user_token_b.mint == pool.mint_b @ AmmError::MintMismatch,
        constraint = user_token_b.owner == user.key() @ AmmError::OwnerMismatch,
    )]
    pub user_token_b: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[event]
pub struct PoolInitialized {
    pub pool: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub lp_mint: Pubkey,
    pub fee_bps: u16,
}

#[event]
pub struct LiquidityAdded {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount_a: u64,
    pub amount_b: u64,
    pub lp_minted: u64,
}

#[event]
pub struct LiquidityRemoved {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount_a: u64,
    pub amount_b: u64,
    pub lp_burned: u64,
}

#[event]
pub struct Swapped {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub a_to_b: bool,
    pub amount_in: u64,
    pub amount_out: u64,
}

#[error_code]
pub enum AmmError {
    #[msg("El fee supera el maximo permitido (1000 bps)")]
    FeeTooHigh,
    #[msg("Los mints deben ir en orden canonico (mint_a < mint_b)")]
    WrongMintOrder,
    #[msg("El monto debe ser mayor que cero")]
    ZeroAmount,
    #[msg("El primer deposito debe producir al menos MINIMUM_LIQUIDITY cuotas")]
    InsufficientInitialLiquidity,
    #[msg("El deposito no alcanza para acunar ninguna cuota LP")]
    InsufficientLiquidityMinted,
    #[msg("El resultado quedo por debajo del minimo aceptado (slippage)")]
    SlippageExceeded,
    #[msg("El pool no tiene liquidez")]
    EmptyPool,
    #[msg("El swap no produce salida (monto demasiado chico)")]
    InsufficientOutput,
    #[msg("Overflow en la aritmetica del pool")]
    MathOverflow,
    #[msg("El mint del token account no coincide con el del pool")]
    MintMismatch,
    #[msg("El owner del token account no es el esperado")]
    OwnerMismatch,
}
