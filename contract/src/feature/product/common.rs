use near_sdk::AccountId;
use sweat_jar_model::data::product::{Product, ProductId};

use crate::{env, Contract};

impl Contract {
    pub fn get_products_for_account(
        &self,
        account_id: &AccountId,
        filter: Option<impl Fn(&Product) -> bool>,
    ) -> Vec<Product> {
        let products = self
            .get_account(account_id)
            .jars
            .keys()
            .map(|product_id| self.get_product(product_id));

        if let Some(filter) = filter {
            products.filter(filter).collect()
        } else {
            products.collect()
        }
    }

    // UnorderedMap doesn't have cache and deserializes `Product` on each get
    // This cached getter significantly reduces gas usage
    #[cfg(not(any(test, feature = "replay-engine")))]
    #[mutants::skip]
    pub(crate) fn get_product(&self, product_id: &ProductId) -> Product {
        self.products_cache
            .borrow_mut()
            .entry(product_id.clone())
            .or_insert_with(|| {
                self.products
                    .get(product_id)
                    .unwrap_or_else(|| env::panic_str(format!("Product {product_id} is not found").as_str()))
            })
            .clone()
    }

    // We should avoid this caching behaviour in tests though
    #[cfg(any(test, feature = "replay-engine"))]
    pub(crate) fn get_product(&self, product_id: &ProductId) -> Product {
        self.products
            .get(product_id)
            .unwrap_or_else(|| env::panic_str(format!("Product {product_id} is not found").as_str()))
    }
}
