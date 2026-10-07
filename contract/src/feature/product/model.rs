#[cfg(test)]
pub mod test_utils {

    use near_sdk::json_types::U128;
    use rstest::fixture;
    use sweat_jar_model::{
        data::product::{
            Apy, Cap, FixedProductTerms, FlexibleProductTerms, Product, ProductId, ScoreBasedProductTerms, Terms,
            TieredScoreBasedProductTerms, WithdrawalFee,
        },
        signer::test_utils::MessageSigner,
        ConfigurableValue, TokenAmount, ValueTier, MS_IN_DAY, MS_IN_HOUR, MS_IN_YEAR,
    };
    use sweat_jar_primitives::UDecimal;

    use crate::common::testing::TokenUtils;

    /// Default product name. If product name wasn't specified it will have this name.
    pub const DEFAULT_PRODUCT_NAME: &str = "product";

    pub struct ProtectedProduct {
        pub product: Product,
        pub signer: MessageSigner,
    }

    #[fixture]
    pub fn tiered_score_based_product(product: Product) -> Product {
        product
            .with_id("product_ts_1_year_20_10_cap".to_string())
            .with_terms(Terms::TieredScoreBased(TieredScoreBasedProductTerms {
                lockup_term: (365 * MS_IN_DAY).into(),
                score_cap: ConfigurableValue::Tier(ValueTier {
                    default: 20_000,
                    fallback: 10_000,
                }),
            }))
    }

    #[fixture]
    pub fn product_fixed(
        #[default(365)] term_in_days: u64,
        #[default("product_fixed")] id: &str,
        product: Product,
    ) -> Product {
        product
            .with_id(id.to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: (term_in_days * MS_IN_DAY).into(),
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_1_year_12_percent(product: Product) -> Product {
        product
            .with_id("product_1_year_12_percent".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_1_year_20_percent(product: Product) -> Product {
        product
            .with_id("product_1_year_20_percent".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                apy: Apy::Constant(UDecimal::new(20_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_flexible_10_percent(product: Product) -> Product {
        product
            .with_id("product_flexible_10_percent".to_string())
            .with_terms(Terms::Flexible(FlexibleProductTerms {
                apy: Apy::Constant(UDecimal::new(10_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_flexible_12_percent(product: Product) -> Product {
        product
            .with_id("product_flexible_12_percent".to_string())
            .with_terms(Terms::Flexible(FlexibleProductTerms {
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_1_year_12_percent_with_fixed_fee(#[default(100)] fee: TokenAmount, product: Product) -> Product {
        product
            .with_id("product_1_year_12_percent_with_fixed_fee".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
            .with_withdrawal_fee(WithdrawalFee::Fix(U128(fee)))
            .with_cap(1_000, 1_000_000_000u128.to_otto())
    }

    #[fixture]
    pub fn product_1_year_12_percent_with_invalid_fixed_fee(product: Product) -> Product {
        product
            .with_id("product_1_year_12_percent_with_invalid_fixed_fee".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
            .with_withdrawal_fee(WithdrawalFee::Fix(U128(2_000)))
            .with_cap(1_000, 1_000_000_000u128.to_otto())
    }

    #[fixture]
    pub fn product_1_year_12_percent_with_percent_fee(
        #[default(UDecimal::new(10_000, 5))] fee: UDecimal,
        product: Product,
    ) -> Product {
        product
            .with_id("product_1_year_12_percent_with_percent_fee".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
            .with_withdrawal_fee(WithdrawalFee::Percent(fee))
            .with_cap(1_000, 1_000_000_000u128.to_otto())
    }

    #[fixture]
    pub fn product_1_year_12_percent_with_invalid_percent_fee(product: Product) -> Product {
        product
            .with_id("product_1_year_12_percent_with_invalid_percent_fee".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                apy: Apy::Constant(UDecimal::new(12_000, 5)),
            }))
            .with_withdrawal_fee(WithdrawalFee::Percent(UDecimal::new(100, 0)))
            .with_cap(1_000, 1_000_000_000u128.to_otto())
    }

    #[fixture]
    pub fn product_2_years_10_percent(product: Product) -> Product {
        product
            .with_id("product_2_years_10_percent".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: (2 * MS_IN_YEAR).into(),
                apy: Apy::Constant(UDecimal::new(10_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_3_years_20_percent(product: Product) -> Product {
        product
            .with_id("product_3_years_20_percent".to_string())
            .with_terms(Terms::Fixed(FixedProductTerms {
                lockup_term: (3 * MS_IN_YEAR).into(),
                apy: Apy::Constant(UDecimal::new(20_000, 5)),
            }))
    }

    #[fixture]
    pub fn product_1_year_12_cap_score_based(product: Product) -> Product {
        product
            .with_id("product_1_year_12_cap_score_based".to_string())
            .with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                score_cap: 12_000,
            }))
    }

    #[fixture]
    pub fn product_1_year_30_cap_score_based_protected(
        #[from(protected_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    ) -> ProtectedProduct {
        ProtectedProduct {
            product: product
                .with_id("product_1_year_30_cap_score_based_protected".to_string())
                .with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                    lockup_term: MS_IN_YEAR.into(),
                    score_cap: 30_000,
                }))
                .with_public_key(signer.public_key().into()),
            signer,
        }
    }

    #[fixture]
    pub fn product_steps_365d_20000_score_cap(product: Product) -> Product {
        product
            .with_id("steps_365d_20000_score_cap".to_string())
            .with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                score_cap: 20_000,
            }))
    }

    #[fixture]
    pub fn product_7_days_18_cap_score_based(product: Product) -> Product {
        product
            .with_id("product_7_days_18_cap_score_based".to_string())
            .with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                lockup_term: (7 * MS_IN_DAY).into(),
                score_cap: 18_000,
            }))
    }

    #[fixture]
    pub fn product_7_days_20_cap_score_based(product: Product) -> Product {
        product
            .with_id("product_7_days_20_cap_score_based".to_string())
            .with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                lockup_term: (7 * MS_IN_DAY).into(),
                score_cap: 20_000,
            }))
    }

    #[fixture]
    pub fn product_10_days_20_cap_score_based(product: Product) -> Product {
        product
            .with_id("product_10_days_10_cap_score_based".to_string())
            .with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                lockup_term: (10 * MS_IN_DAY).into(),
                score_cap: 20_000,
            }))
    }

    #[fixture]
    pub fn product_1_year_apy_10_percent(product: Product) -> Product {
        product
            .with_id("product_1_year_apy_10_percent".to_string())
            .with_terms(terms(Apy::Constant(UDecimal::new(10_000, 5))))
    }

    #[fixture]
    pub fn product_1_year_apy_20_percent(product: Product) -> Product {
        product
            .with_id("product_1_year_apy_20_percent".to_string())
            .with_terms(terms(Apy::Constant(UDecimal::new(20_000, 5))))
    }

    #[fixture]
    pub fn product_1_year_apy_7_percent_protected(
        #[from(protected_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    ) -> ProtectedProduct {
        ProtectedProduct {
            product: product
                .with_id("product_1_year_apy_7_percent_protected".to_string())
                .with_terms(terms(Apy::Constant(UDecimal::new(7_000, 5))))
                .with_public_key(signer.public_key().into()),
            signer,
        }
    }

    #[fixture]
    pub fn product_1_year_apy_downgradable_20_10_percent_protected(
        #[from(protected_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    ) -> ProtectedProduct {
        ProtectedProduct {
            product: product
                .with_id("product_1_year_apy_downgradable_20_10_percent_protected".to_string())
                .with_terms(terms(Apy::Tier(ValueTier {
                    default: UDecimal::new(20_000, 5),
                    fallback: UDecimal::new(10_000, 5),
                })))
                .with_public_key(signer.public_key().into()),
            signer,
        }
    }

    #[fixture]
    pub fn product_1_hour_apy_downgradable_23_10_percent_protected(
        #[from(protected_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    ) -> ProtectedProduct {
        ProtectedProduct {
            product: product
                .with_id("product_1_hour_apy_downgradable_23_10_percent_protected".to_string())
                .with_terms(Terms::Fixed(FixedProductTerms {
                    apy: Apy::Tier(ValueTier {
                        default: UDecimal::new(23, 2),
                        fallback: UDecimal::new(10, 2),
                    }),
                    lockup_term: MS_IN_HOUR.into(),
                }))
                .with_public_key(signer.public_key().into()),
            signer,
        }
    }

    #[fixture]
    pub fn product_disabled(product: Product) -> Product {
        product.with_id("product_disabled".to_string()).with_enabled(false)
    }

    #[fixture]
    pub fn product(
        #[default(DEFAULT_PRODUCT_NAME.to_string())] id: ProductId,
        #[default(cap(0, 1_000_000_000u128.to_otto()))] cap: Cap,
        #[default(terms(downgradable_apy()))] terms: Terms,
    ) -> Product {
        Product {
            id,
            cap,
            terms,
            withdrawal_fee: None,
            public_key: None,
            is_enabled: true,
        }
    }

    #[fixture]
    pub fn protected_product(
        #[default(DEFAULT_PRODUCT_NAME.to_string())] id: ProductId,
        product: Product,
        message_signer: MessageSigner,
    ) -> ProtectedProduct {
        ProtectedProduct {
            product: product
                .with_id(id)
                .with_public_key(message_signer.public_key().into())
                .with_cap(0, 100_000_000_000)
                .with_terms(Terms::Fixed(FixedProductTerms {
                    apy: Apy::Tier(ValueTier {
                        default: UDecimal::new(20, 2),
                        fallback: UDecimal::new(10, 2),
                    }),
                    lockup_term: MS_IN_YEAR.into(),
                })),
            signer: message_signer,
        }
    }

    #[fixture]
    pub fn message_signer() -> MessageSigner {
        MessageSigner::new()
    }

    #[fixture]
    pub fn protected_score_based_product(
        #[default(DEFAULT_PRODUCT_NAME.to_string())] id: ProductId,
        product: Product,
        message_signer: MessageSigner,
    ) -> ProtectedProduct {
        let ProtectedProduct { product, signer } = protected_product(id, product, message_signer);

        ProtectedProduct {
            product: product.with_terms(Terms::ScoreBased(ScoreBasedProductTerms {
                lockup_term: MS_IN_YEAR.into(),
                score_cap: 20_000,
            })),
            signer,
        }
    }

    #[fixture]
    pub fn terms(#[from(downgradable_apy)] apy: Apy) -> Terms {
        Terms::Fixed(FixedProductTerms {
            lockup_term: MS_IN_YEAR.into(),
            apy,
        })
    }

    #[fixture]
    pub fn cap(#[default(0)] min: TokenAmount, #[default(1_000_000_000u128.to_otto())] max: TokenAmount) -> Cap {
        Cap::new(min, max)
    }

    #[fixture]
    pub fn constant_apy() -> Apy {
        Apy::Constant(UDecimal::new(12, 2))
    }

    #[fixture]
    pub fn downgradable_apy() -> Apy {
        Apy::Tier(ValueTier {
            default: UDecimal::new(20, 2),
            fallback: UDecimal::new(10, 2),
        })
    }

    pub(crate) trait ProductBuilder {
        fn with_id(self, id: ProductId) -> Self;
        fn with_enabled(self, enabled: bool) -> Self;
        fn with_terms(self, terms: Terms) -> Self;
        fn with_public_key(self, public_key: Option<Vec<u8>>) -> Self;
        fn with_withdrawal_fee(self, fee: WithdrawalFee) -> Self;
        fn with_cap(self, min: TokenAmount, max: TokenAmount) -> Self;
    }

    impl ProductBuilder for Product {
        #[must_use]
        fn with_id(mut self, id: ProductId) -> Self {
            self.id = id;
            self
        }

        #[must_use]
        fn with_enabled(mut self, enabled: bool) -> Self {
            self.is_enabled = enabled;
            self
        }

        #[must_use]
        fn with_terms(mut self, terms: Terms) -> Self {
            self.terms = terms;
            self
        }

        #[must_use]
        fn with_public_key(mut self, public_key: Option<Vec<u8>>) -> Self {
            self.public_key = public_key.map(Into::into);
            self
        }

        #[must_use]
        fn with_withdrawal_fee(mut self, fee: WithdrawalFee) -> Self {
            self.withdrawal_fee = Some(fee);
            self
        }

        #[must_use]
        fn with_cap(mut self, min: TokenAmount, max: TokenAmount) -> Self {
            self.cap = Cap::new(min, max);
            self
        }
    }

    pub(crate) trait BaseApy {
        fn get_base_apy(&self) -> &Apy;
    }

    impl BaseApy for Product {
        fn get_base_apy(&self) -> &Apy {
            match &self.terms {
                Terms::Fixed(value) => &value.apy,
                Terms::Flexible(value) => &value.apy,
                Terms::ScoreBased(_) => panic!("No APY for a score based product"),
                Terms::TieredScoreBased(_) => panic!("No APY for a score based product"),
            }
        }
    }
}
