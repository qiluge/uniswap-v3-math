use crate::error::UniswapV3MathError;
use crate::liquidity_math;
use crate::swap_math;
use crate::tick_bitmap;
use crate::tick_math;
use ethers::prelude::*;
use hashbrown::HashMap;

#[derive(Clone)]
pub struct TickInfo {
    pub index: i32,
    // liquidity gross
    pub l_gross: u128,
    // liquidity net
    pub l_net: i128,
}

// 代表pool的当前状况
pub struct Slot0 {
    pub sqrt_price: U256,
    pub liquidity: u128,
    pub tick: i32,
}

pub struct SwapResult {
    pub amount0_delta: I256,
    pub amount1_delta: I256,
    pub sqrt_price_after: U256,
    pub liquidity_after: u128,
    pub tick_after: i32,
}

struct SwapState {
    amount_specified_remaining: I256,
    amount_calculated: I256,
    sqrt_price_x96: U256,
    tick: i32,
    liquidity: u128,
}

#[derive(Default)]
struct StepComputations {
    sqrt_price_start_x96: U256,
    tick_next: i32,
    initialized: bool,
    sqrt_price_next_x96: U256,
    amount_in: U256,
    amount_out: U256,
    fee_amount: U256,
}

pub fn swap(
    ticks: &HashMap<i32, TickInfo>,
    tick_bitmap: &HashMap<i16, U256>,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: U256,
    slot0: &Slot0,
    fee: u32,
) -> Result<SwapResult, UniswapV3MathError> {
    if sqrt_price_limit <= tick_math::MIN_SQRT_RATIO {
        return Err(UniswapV3MathError::SplM);
    }
    if sqrt_price_limit >= tick_math::MAX_SQRT_RATIO {
        return Err(UniswapV3MathError::SpuM);
    }
    if zero_for_one {
        if sqrt_price_limit >= slot0.sqrt_price {
            return Err(UniswapV3MathError::SplC);
        }
    } else {
        if sqrt_price_limit <= slot0.sqrt_price {
            return Err(UniswapV3MathError::SpuC);
        }
    }
    let exact_input = amount_specified.is_positive();
    let mut state = SwapState {
        amount_specified_remaining: amount_specified,
        amount_calculated: I256::zero(),
        sqrt_price_x96: slot0.sqrt_price,
        tick: slot0.tick,
        liquidity: slot0.liquidity,
    };
    while !state.amount_specified_remaining.is_zero() && state.sqrt_price_x96 != sqrt_price_limit {
        let mut step = StepComputations::default();
        step.sqrt_price_start_x96 = state.sqrt_price_x96;
        (step.tick_next, step.initialized) = tick_bitmap::next_initialized_tick_within_one_word(
            tick_bitmap,
            state.tick,
            tick_spacing,
            zero_for_one,
        )?;
        if step.tick_next < tick_math::MIN_TICK {
            step.tick_next = tick_math::MIN_TICK;
        } else if step.tick_next > tick_math::MAX_TICK {
            step.tick_next = tick_math::MAX_TICK;
        }
        step.sqrt_price_next_x96 = tick_math::get_sqrt_ratio_at_tick(step.tick_next)?;
        let hit_to_limit = if zero_for_one {
            // 卖出
            step.sqrt_price_next_x96 < sqrt_price_limit // 下一个tick的价格比limit低
        } else {
            // 买入
            step.sqrt_price_next_x96 > sqrt_price_limit // 下一个tick的价格比limit高
        };
        let target_price = if hit_to_limit {
            sqrt_price_limit
        } else {
            step.sqrt_price_next_x96
        };
        // compute values to swap to the target tick, price limit, or point where input/output amount is exhausted
        (
            state.sqrt_price_x96,
            step.amount_in,
            step.amount_out,
            step.fee_amount,
        ) = swap_math::compute_swap_step(
            state.sqrt_price_x96,
            target_price,
            state.liquidity,
            state.amount_specified_remaining,
            fee,
        )?;
        if exact_input {
            state.amount_specified_remaining =
                state.amount_specified_remaining - I256::from_raw(step.amount_in + step.fee_amount);
            state.amount_calculated = state.amount_calculated - I256::from_raw(step.amount_out);
        } else {
            state.amount_specified_remaining =
                state.amount_specified_remaining + I256::from_raw(step.amount_out);
            state.amount_calculated =
                state.amount_calculated + I256::from_raw(step.amount_in + step.fee_amount);
        }
        // 不计算protocol fee
        if state.sqrt_price_x96 == step.sqrt_price_next_x96 {
            if step.initialized {
                // initialized tick一定存在于ticks里
                let mut l_net = ticks.get(&step.tick_next).unwrap().l_net;
                if zero_for_one {
                    l_net = -1 * l_net;
                }
                state.liquidity = liquidity_math::add_delta(state.liquidity, l_net)?;
            }
            if zero_for_one {
                state.tick = step.tick_next - 1
            } else {
                state.tick = step.tick_next
            }
        } else if state.sqrt_price_x96 != step.sqrt_price_start_x96 {
            state.tick = tick_math::get_tick_at_sqrt_ratio(state.sqrt_price_x96)?;
        }
    }
    let amount0_delta;
    let amount1_delta;
    if zero_for_one == exact_input {
        amount0_delta = amount_specified - state.amount_specified_remaining;
        amount1_delta = state.amount_calculated;
    } else {
        amount0_delta = state.amount_calculated;
        amount1_delta = amount_specified - state.amount_specified_remaining;
    }
    return Ok(SwapResult {
        amount0_delta,
        amount1_delta,
        sqrt_price_after: state.sqrt_price_x96,
        liquidity_after: state.liquidity,
        tick_after: state.tick,
    });
}
