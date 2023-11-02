use crate::error::UniswapV3MathError;
use crate::swap::TickInfo;
use crate::tick_bitmap;
use crate::tick_math;
use ethers::prelude::U256;
use hashbrown::HashMap;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref Q96: f64 = 2f64.powi(96);
    pub static ref Q192: f64 = 2f64.powi(192);
}
// 代表pool的当前状况
pub struct Slot0 {
    pub sqrt_price: f64,
    pub liquidity: u128,
    pub tick: i32,
}

struct SwapState {
    amount_specified_remaining: f64,
    amount_calculated: f64,
    sqrt_price_x96: f64,
    tick: i32,
    liquidity: f64,
}

#[derive(Default)]
struct StepComputations {
    sqrt_price_start_x96: f64,
    tick_next: i32,
    initialized: bool,
    sqrt_price_next_x96: f64,
    amount_in: f64,
    amount_out: f64,
    fee_amount: f64,
}

#[derive(Default)]
pub struct SwapResult {
    pub amount0_delta: f64,
    pub amount1_delta: f64,
    pub sqrt_price_after: f64,
    pub liquidity_after: f64,
    pub tick_after: i32,
}

pub fn swap(
    ticks: &HashMap<i32, TickInfo>,
    tick_bitmap: &HashMap<i16, U256>,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: f64,
    sqrt_price_limit_x96: f64,
    slot0: &Slot0,
    fee: f64,
    token0_decimals_factor: f64,
    token1_decimals_factor: f64,
) -> Result<SwapResult, UniswapV3MathError> {
    if ticks.len() == 0 {
        return Ok(SwapResult::default());
    }
    // if sqrt_price_limit_x96 <= tick_math::MIN_SQRT_RATIO {
    //     return Err(UniswapV3MathError::SplM);
    // }
    // if sqrt_price_limit_x96 >= tick_math::MAX_SQRT_RATIO {
    //     return Err(UniswapV3MathError::SpuM);
    // }
    // if zero_for_one {
    //     if sqrt_price_limit_x96 >= slot0.sqrt_price {
    //         return Err(UniswapV3MathError::SplC);
    //     }
    // } else {
    //     if sqrt_price_limit_x96 <= slot0.sqrt_price {
    //         return Err(UniswapV3MathError::SpuC);
    //     }
    // }
    let exact_input = amount_specified > 0f64;
    let amount_specified_remaining = if zero_for_one {
        if amount_specified > 0f64 {
            // input token 0
            amount_specified * token0_decimals_factor
        } else {
            // output token1
            amount_specified * token1_decimals_factor
        }
    } else {
        if amount_specified > 0f64 {
            // input token 1
            amount_specified * token1_decimals_factor
        } else {
            // output token 0
            amount_specified * token0_decimals_factor
        }
    };
    let mut state = SwapState {
        amount_specified_remaining,
        amount_calculated: 0f64,
        sqrt_price_x96: slot0.sqrt_price.to_string().parse::<f64>().unwrap(),
        tick: slot0.tick,
        liquidity: slot0.liquidity as f64,
    };
    let mut exhausted = false;
    loop {
        // 流动性归零或者amount消耗完
        if exhausted || state.amount_specified_remaining == 0f64 || state.liquidity <= 0f64 {
            break;
        }
        // 价格到了限价
        if zero_for_one {
            if state.sqrt_price_x96 <= sqrt_price_limit_x96 {
                break;
            }
        } else {
            if state.sqrt_price_x96 >= sqrt_price_limit_x96 {
                break;
            }
        }
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
        step.sqrt_price_next_x96 = (1.0001f64.powi(step.tick_next) * *Q192).sqrt();
        let hit_to_limit = if zero_for_one {
            // 卖出
            step.sqrt_price_next_x96 < sqrt_price_limit_x96 // 下一个tick的价格比limit低
        } else {
            // 买入
            step.sqrt_price_next_x96 > sqrt_price_limit_x96 // 下一个tick的价格比limit高
        };
        let target_price = if hit_to_limit {
            sqrt_price_limit_x96
        } else {
            step.sqrt_price_next_x96
        };
        (
            state.sqrt_price_x96,
            step.amount_in,
            step.amount_out,
            step.fee_amount,
            exhausted,
        ) = compute_swap_step(
            state.sqrt_price_x96,
            target_price,
            state.liquidity,
            state.amount_specified_remaining,
            fee,
        );
        if exact_input {
            state.amount_specified_remaining =
                state.amount_specified_remaining - (step.amount_in + step.fee_amount);
            state.amount_calculated = state.amount_calculated - step.amount_out;
        } else {
            state.amount_specified_remaining = state.amount_specified_remaining + step.amount_out;
            state.amount_calculated = state.amount_calculated + (step.amount_in + step.fee_amount);
        }
        // 价格刚好打到下一个tick上
        if state.sqrt_price_x96 == step.sqrt_price_next_x96 {
            // 如果tick初始化了，则需要更新流动性
            if step.initialized {
                let mut l_net = ticks.get(&step.tick_next).unwrap().l_net as f64;
                if zero_for_one {
                    l_net = -1f64 * l_net;
                }
                state.liquidity = state.liquidity + l_net;
            }
            if zero_for_one {
                state.tick = step.tick_next - 1
            } else {
                state.tick = step.tick_next
            }
        }
        // 这一段省略，因为价格没有超出当前这一段，循环也已经结束了，我们不用更新tick
        // else if state.sqrt_price_x96 != step.sqrt_price_start_x96 {
        //     state.tick = ((state.sqrt_price_x96*state.sqrt_price_x96/Q192).log(1.0001) as i32) / tick_spacing * tick_spacing;
        // }
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
        amount0_delta: amount0_delta / token0_decimals_factor,
        amount1_delta: amount1_delta / token1_decimals_factor,
        sqrt_price_after: state.sqrt_price_x96,
        liquidity_after: state.liquidity,
        tick_after: state.tick,
    });
}

// 最后一个值表示amount_remaining是否耗尽了
// 由于是浮点计算，外层扣除amount_remaining时会有误差，所以这个方法额外返回一个bool值辅助外层调用，判断是否应该结束swap
fn compute_swap_step(
    sqrt_p_current: f64,
    sqrt_p_target: f64,
    l: f64,
    amount_remaining: f64,
    fee_pips: f64,
) -> (f64, f64, f64, f64, bool) {
    let zero_for_one = sqrt_p_current >= sqrt_p_target;
    let exact_in = amount_remaining >= 0f64;
    // return values
    let sqrt_p_next;
    let mut amount_in = 0f64;
    let mut amount_out = 0f64;
    let fee_amount;
    let mut max = false;
    let mut exhausted = false;
    if exact_in {
        let amount_remaining_less_fee = amount_remaining * (1f64 - fee_pips);
        // 计算将价格打到target price需要多少input
        amount_in = if zero_for_one {
            get_amount0_delta(sqrt_p_target, sqrt_p_current, l)
        } else {
            get_amount1_delta(sqrt_p_current, sqrt_p_target, l)
        };
        // 如果input的数量足够，则这一段的swap之后，价格打到了target price
        if amount_remaining_less_fee >= amount_in {
            sqrt_p_next = sqrt_p_target;
            max = true;
        } else {
            // 数量不够，则计算这些input能将价格推到的新的价格
            sqrt_p_next = get_sqrt_price_from_input(
                zero_for_one,
                amount_remaining_less_fee,
                sqrt_p_current,
                l,
            );
            exhausted = true;
        }
    } else {
        // 计算将价格打到target price能swap出多少output
        amount_out = if zero_for_one {
            get_amount1_delta(sqrt_p_target, sqrt_p_current, l)
        } else {
            get_amount0_delta(sqrt_p_current, sqrt_p_target, l)
        };
        // 能swap出的output数量比要求的要少，说明需要继续往下一段交易
        if -amount_remaining >= amount_out {
            // 就把价格打到target price
            sqrt_p_next = sqrt_p_target;
            max = true;
        } else {
            // 否则计算新的价格
            sqrt_p_next =
                get_sqrt_price_from_output(zero_for_one, -amount_remaining, sqrt_p_current, l);
            exhausted = true;
        }
    }
    if zero_for_one {
        if !max || !exact_in {
            amount_in = get_amount0_delta(sqrt_p_next, sqrt_p_current, l);
        }
        if !max || exact_in {
            amount_out = get_amount1_delta(sqrt_p_next, sqrt_p_current, l);
        }
    } else {
        if !max || !exact_in {
            amount_in = get_amount1_delta(sqrt_p_current, sqrt_p_next, l);
        }
        if !max || exact_in {
            amount_out = get_amount0_delta(sqrt_p_current, sqrt_p_next, l);
        }
    }
    // cap the output amount to not exceed the remaining output amount
    if !exact_in && amount_out > -amount_remaining {
        amount_out = -amount_remaining
    }
    if exact_in && !max {
        // we didn't reach the target, so take the remainder of the maximum input as fee
        fee_amount = amount_remaining - amount_in;
    } else {
        fee_amount = amount_in * fee_pips / (1f64 - fee_pips);
    }
    return (sqrt_p_next, amount_in, amount_out, fee_amount, exhausted);
}

fn get_amount0_delta(sqrt_price_lower: f64, sqrt_price_upper: f64, sqrt_l: f64) -> f64 {
    return sqrt_l * *Q96 * (sqrt_price_upper - sqrt_price_lower)
        / (sqrt_price_lower * sqrt_price_upper);
}

fn get_amount1_delta(sqrt_price_lower: f64, sqrt_price_upper: f64, sqrt_l: f64) -> f64 {
    return sqrt_l * (sqrt_price_upper - sqrt_price_lower) / *Q96;
}

fn get_sqrt_price_from_input(
    zero_for_one: bool,
    amount_in: f64,
    sqrt_price_current: f64,
    sqrt_l: f64,
) -> f64 {
    if zero_for_one {
        // amountIn是token0，priceAfter是更低的价格，求sqrtPriceLower
        // sqrtL*Q96*/sqrtPriceLower - sqrtL*Q96*/sqrtPriceUpper = amountIn
        let sqrt_l = sqrt_l * *Q96;
        return sqrt_l * sqrt_price_current / (sqrt_l + amount_in * sqrt_price_current);
    } else {
        // amountIn是token1，priceAfter是更高的价格，求sqrtPriceUpper
        // sqrt_l * (sqrt_price_upper - sqrt_price_lower) / Q96 = amountIn
        return sqrt_price_current + amount_in * *Q96 / sqrt_l;
    }
}

fn get_sqrt_price_from_output(
    zero_for_one: bool,
    amount_out: f64,
    sqrt_price_current: f64,
    sqrt_l: f64,
) -> f64 {
    if zero_for_one {
        // amount_out是token1，priceAfter是更低的价格，求sqrtPriceLower
        // sqrt_l * (sqrt_price_upper - sqrt_price_lower) / Q96 = amountOut
        return sqrt_price_current - amount_out * *Q96 / sqrt_l;
    } else {
        // amount_out是token0，priceAfter是更高的价格，求sqrtPriceUpper
        // sqrtL*Q96*/sqrtPriceLower - sqrtL*Q96*/sqrtPriceUpper = amountOut
        let sqrt_l = sqrt_l * *Q96;
        return sqrt_l * sqrt_price_current / (sqrt_l - amount_out * sqrt_price_current);
    }
}
