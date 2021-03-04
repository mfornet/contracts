use std::cmp::min;

use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::collections::LookupMap;
use near_sdk::json_types::{ValidAccountId, U128};
use near_sdk::{ext_contract, AccountId, Balance, Gas};
use uint::construct_uint;

const FEE_DIVISOR: u32 = 1_000;
const MAX_NUM_TOKENS: usize = 10;
const INIT_SHARES_SUPPLY: u128 = 1_000_000_000_000_000_000_000;

pub const GAS_FOR_FT_TRANSFER: Gas = 10_000_000_000_000;
pub const NO_DEPOSIT: Balance = 0;

construct_uint! {
    /// 256-bit unsigned integer.
    pub struct U256(4);
}

#[ext_contract(ext_fungible_token)]
pub trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

pub fn add_to_collection(c: &mut LookupMap<AccountId, Balance>, key: &String, amount: Balance) {
    let prev_amount = c.get(key).unwrap_or(0);
    c.insert(key, &(prev_amount + amount));
}

#[derive(BorshSerialize, BorshDeserialize)]
pub struct Pool {
    /// List of tokens in the pool.
    token_account_ids: Vec<AccountId>,
    /// How much NEAR this contract has.
    amounts: Vec<Balance>,
    /// Fee charged for swap.
    fee: u32,
    /// Shares of the pool by liquidity providers.
    shares: LookupMap<AccountId, Balance>,
    /// Total number of shares.
    shares_total_supply: Balance,
}

impl Pool {
    pub fn new(id: u32, token_account_ids: Vec<ValidAccountId>, fee: u32) -> Self {
        assert!(fee < FEE_DIVISOR, "ERR_FEE_TOO_LARGE");
        assert!(
            token_account_ids.len() < MAX_NUM_TOKENS,
            "ERR_TOO_MANY_TOKENS"
        );
        Self {
            token_account_ids: token_account_ids.iter().map(|a| a.clone().into()).collect(),
            amounts: vec![0u128; token_account_ids.len()],
            fee,
            shares: LookupMap::new(format!("s{}", id).into_bytes()),
            shares_total_supply: 0,
            // liquidity_amounts: LookupMap::new(format!("l{}", id).into_bytes()),
        }
    }

    pub fn tokens(&self) -> &[AccountId] {
        &self.token_account_ids
    }

    /// Adds token to liquidity pool.
    pub fn add_liquidity(&mut self, sender_id: AccountId, amounts: Vec<Balance>) -> Balance {
        assert_eq!(
            amounts.len(),
            self.token_account_ids.len(),
            "ERR_WRONG_TOKEN_COUNT"
        );
        let shares = if self.shares_total_supply > 0 {
            let mut fair_supply = U256::max_value();
            for i in 0..self.token_account_ids.len() {
                assert!(amounts[i] > 0, "ERR_AMOUNT_ZERO");
                fair_supply = min(
                    fair_supply,
                    U256::from(amounts[i]) * U256::from(self.shares_total_supply) / self.amounts[i],
                );
            }
            for i in 0..self.token_account_ids.len() {
                let amount = U256::from(self.amounts[i]) * fair_supply
                    / U256::from(self.shares_total_supply);
                self.amounts[i] += amount.as_u128();
            }
            fair_supply.as_u128()
        } else {
            for i in 0..self.token_account_ids.len() {
                self.amounts[i] += amounts[i];
            }
            INIT_SHARES_SUPPLY
        };
        self.shares_total_supply += shares;
        add_to_collection(&mut self.shares, &sender_id, shares);
        shares
    }

    /// Removes given number of shares from the pool and returns amounts to the parent.
    pub fn remove_liquidity(
        &mut self,
        sender_id: &AccountId,
        shares: Balance,
        min_amounts: Vec<Balance>,
    ) -> Vec<Balance> {
        let prev_shares_amount = self.shares.get(&sender_id).expect("ERR_NO_SHARES");
        assert!(prev_shares_amount >= shares, "ERR_NOT_ENOUGH_SHARES");
        let mut result = vec![];
        for i in 0..self.token_account_ids.len() {
            let amount = (U256::from(self.amounts[i]) * U256::from(shares)
                / U256::from(self.shares_total_supply))
            .as_u128();
            assert!(amount >= min_amounts[i], "ERR_MIN_AMOUNT");
            self.amounts[i] -= amount;
            result.push(amount);
        }
        if prev_shares_amount == shares {
            self.shares.remove(&sender_id);
        } else {
            self.shares
                .insert(&sender_id, &(prev_shares_amount - shares));
        }
        self.shares_total_supply -= shares;
        result
    }

    fn token_index(&self, token_id: &AccountId) -> usize {
        self.token_account_ids
            .iter()
            .position(|id| id == token_id)
            .expect("ERR_MISSING_TOKEN")
    }

    fn get_return_idx(&self, token_in: usize, amount_in: Balance, token_out: usize) -> Balance {
        let in_balance = U256::from(self.amounts[token_in]);
        let out_balance = U256::from(self.amounts[token_out]);
        assert!(
            in_balance > U256::zero()
                && out_balance > U256::zero()
                && token_in != token_out
                && amount_in > 0,
            "ERR_INVALID"
        );
        let amount_with_fee = U256::from(amount_in) * U256::from(FEE_DIVISOR - self.fee);
        (amount_with_fee * out_balance / (U256::from(FEE_DIVISOR) * in_balance + amount_with_fee))
            .as_u128()
    }

    /// Returns how much token you will receive if swap `token_amount_in` of `token_in` for `token_out`.
    pub fn get_return(
        &self,
        token_in: ValidAccountId,
        amount_in: Balance,
        token_out: ValidAccountId,
    ) -> Balance {
        self.get_return_idx(
            self.token_index(token_in.as_ref()),
            amount_in,
            self.token_index(token_out.as_ref()),
        )
    }

    /// Swap `token_amount_in` of `token_in` token into `token_out` and return how much was received.
    /// Assuming that `token_amount_in` was already received from `sender_id`.
    pub fn swap(
        &mut self,
        sender_id: &AccountId,
        token_in: &AccountId,
        amount_in: Balance,
        token_out: &AccountId,
        min_amount_out: Balance,
    ) -> Balance {
        let in_idx = self.token_index(token_in);
        let out_idx = self.token_index(token_out);
        let amount_out = self.get_return_idx(in_idx, amount_in, out_idx);
        assert!(amount_out >= min_amount_out, "ERR_MIN_AMOUNT");

        self.amounts[in_idx] += amount_in;
        self.amounts[out_idx] -= amount_out;

        ext_fungible_token::ft_transfer(
            sender_id.clone(),
            U128(amount_out),
            None,
            &self.token_account_ids[out_idx],
            NO_DEPOSIT,
            GAS_FOR_FT_TRANSFER,
        );

        amount_out
    }
}

#[cfg(test)]
mod tests {
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::{testing_env, MockedBlockchain};

    use super::*;

    #[test]
    fn test_pool_swap() {
        let one_near = 10u128.pow(24);
        let mut context = VMContextBuilder::new();
        context.predecessor_account_id(accounts(0));
        testing_env!(context.build());
        let mut pool = Pool::new(0, vec![accounts(1), accounts(2)], 3);
        let num_shares = pool.add_liquidity(accounts(0).into(), vec![5 * one_near, 10 * one_near]);
        pool.swap(
            accounts(0).as_ref(),
            accounts(1).as_ref(),
            one_near,
            accounts(2).as_ref(),
            1,
        );
        pool.remove_liquidity(accounts(0).as_ref(), num_shares, vec![1, 1]);
    }
}