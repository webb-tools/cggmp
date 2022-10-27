use curv::{
	arithmetic::traits::*,
	cryptographic_primitives::hashing::{Digest, DigestExt},
	elliptic::curves::Curve,
	BigInt,
};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use zk_paillier::zkproofs::IncorrectProof;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncSetupParameters<E: Curve, H: Digest + Clone> {
	s: BigInt,
	t: BigInt,
	N_hat: BigInt,
	phantom: PhantomData<(E, H)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncCommonInput<E: Curve, H: Digest + Clone> {
	N0: BigInt,
	NN0: BigInt,
	K: BigInt,
	phantom: PhantomData<(E, H)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncWitness<E: Curve, H: Digest + Clone> {
	k: BigInt,
	rho: BigInt,
	phantom: PhantomData<(E, H)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncProof<E: Curve, H: Digest + Clone> {
	S: BigInt,
	A: BigInt,
	C: BigInt,
	z_1: BigInt,
	z_2: BigInt,
	z_3: BigInt,
	phantom: PhantomData<(E, H)>,
}

impl<E: Curve, H: Digest + Clone> EncProof<E, H> {
	fn prove(
		witness: &EncWitness<E, H>,
		common_input: &EncCommonInput<E, H>,
		setup_parameters: &EncSetupParameters<E, H>,
	) -> Self {
		// Step 1: Sample alpha
		let alpha_upper = BigInt::pow(&BigInt::from(2), crate::utilities::L_PLUS_EPSILON as u32);
		let alpha_lower = BigInt::from(-1).mul(&alpha_upper);
		let alpha = BigInt::sample_range(&alpha_lower, &alpha_upper);

		// Step 2: mu, r, gamma
		let mu_upper = BigInt::mul(
			&setup_parameters.N_hat,
			&BigInt::pow(&BigInt::from(2), crate::utilities::L as u32),
		);
		let mu_lower = BigInt::from(-1).mul(&mu_upper);
		let mu = BigInt::sample_range(&mu_lower, &mu_upper);

		let gamma_upper = BigInt::mul(
			&setup_parameters.N_hat,
			&BigInt::pow(&BigInt::from(2), crate::utilities::L_PLUS_EPSILON as u32),
		);
		let gamma_lower = BigInt::from(-1).mul(&mu_upper);
		let gamma = BigInt::sample_range(&gamma_lower, &gamma_upper);

		let r = sample_relatively_prime_integer(common_input.N0.clone());

		// Step 3: S, A, C
		let S = BigInt::mod_mul(
			&BigInt::mod_pow(&setup_parameters.s, &witness.k, &setup_parameters.N_hat),
			&BigInt::mod_pow(&setup_parameters.t, &mu, &setup_parameters.N_hat),
			&setup_parameters.N_hat,
		);

		let NN0 = BigInt::mul(&common_input.N0, &common_input.N0);
		let mut one_plus_N0 = BigInt::add(&BigInt::from(1), &common_input.N0);
		if alpha < BigInt::zero() {
			one_plus_N0 = BigInt::mod_inv(&one_plus_N0, &NN0).unwrap();
		}
		let A = BigInt::mod_mul(
			&BigInt::mod_pow(&one_plus_N0, &BigInt::abs(&alpha), &NN0),
			&BigInt::mod_pow(&r, &common_input.N0, &NN0),
			&NN0,
		);

		let C = BigInt::mod_mul(
			&BigInt::mod_pow(&setup_parameters.s, &alpha, &setup_parameters.N_hat),
			&BigInt::mod_pow(&setup_parameters.t, &gamma, &setup_parameters.N_hat),
			&setup_parameters.N_hat,
		);

		// Step 4: Hash S, A, C
		let e = H::new().chain_bigint(&S).chain_bigint(&A).chain_bigint(&C).result_bigint();

		// Step 5: Compute z_1, z_2, z_3
		let z_1 = BigInt::add(&alpha, &BigInt::mul(&e, &witness.k));
		let z_2 = BigInt::mod_mul(
			&r,
			&BigInt::mod_pow(&witness.rho, &e, &common_input.N0),
			&common_input.N0,
		);
		let z_3 = BigInt::add(&gamma, &BigInt::mul(&e, &mu));

		Self { S, A, C, z_1, z_2, z_3, phantom: PhantomData }
	}

	fn verify(
		proof: &EncProof<E, H>,
		common_input: &EncCommonInput<E, H>,
		setup_parameters: &EncSetupParameters<E, H>,
	) -> Result<(), IncorrectProof> {
		let e = H::new()
			.chain_bigint(&proof.S)
			.chain_bigint(&proof.A)
			.chain_bigint(&proof.C)
			.result_bigint();

		// Equality Checks
		let NN0 = common_input.NN0.clone();
		let left_1 = BigInt::mod_mul(
			&BigInt::mod_pow(&BigInt::add(&BigInt::from(1), &common_input.N0), &proof.z_1, &NN0),
			&BigInt::mod_pow(&proof.z_2, &common_input.N0, &NN0),
			&NN0,
		);
		let right_1 = BigInt::mod_mul(&proof.A, &BigInt::mod_pow(&common_input.K, &e, &NN0), &NN0);

		let left_2 = BigInt::mod_mul(
			&BigInt::mod_pow(&setup_parameters.s, &proof.z_1, &setup_parameters.N_hat),
			&BigInt::mod_pow(&setup_parameters.t, &proof.z_3, &setup_parameters.N_hat),
			&setup_parameters.N_hat,
		);
		let right_2 = BigInt::mod_mul(
			&proof.C,
			&BigInt::mod_pow(&proof.S, &e, &setup_parameters.N_hat),
			&setup_parameters.N_hat,
		);

		if left_1 != right_1 || left_2 != right_2 {
			return Err(IncorrectProof)
		}

		// Range Check
		let lower_bound_check: bool = &proof.z_1 >=
			&BigInt::from(-1)
				.mul(&BigInt::pow(&BigInt::from(2), crate::utilities::L_PLUS_EPSILON as u32));

		let upper_bound_check =
			&proof.z_1 <= &BigInt::pow(&BigInt::from(2), crate::utilities::L_PLUS_EPSILON as u32);

		if !(lower_bound_check && upper_bound_check) {
			return Err(IncorrectProof)
		}

		Ok(())
	}
}

fn sample_relatively_prime_integer(N: BigInt) -> BigInt {
	let mut sample = BigInt::sample_below(&N);
	while BigInt::gcd(&sample, &N) != BigInt::from(1) {
		sample = BigInt::sample_below(&N);
	}
	sample
}

#[cfg(test)]
mod tests {
	use super::*;
	use curv::elliptic::curves::{secp256_k1::Secp256k1, Scalar};
	use paillier::{DecryptionKey, EncryptionKey, KeyGeneration, Paillier};
	use sha2::Sha256;

	const PAILLIER_KEY_SIZE: usize = 2048;

	fn generate_test_values() -> (
		EncWitness<Secp256k1, Sha256>,
		EncCommonInput<Secp256k1, Sha256>,
		EncSetupParameters<Secp256k1, Sha256>,
	) {
		let (ek_tilde, dk_tilde) = Paillier::keypair_with_modulus_size(PAILLIER_KEY_SIZE).keys();
		let one = BigInt::one();
		let phi = (&dk_tilde.p - &one) * (&dk_tilde.q - &one);
		let r = BigInt::sample_below(&ek_tilde.n);
		let lambda = BigInt::sample_below(&phi);
		let t = BigInt::mod_pow(&r, &BigInt::from(2), &ek_tilde.n);
		let s = BigInt::mod_pow(&t, &lambda, &ek_tilde.n);
		let k = BigInt::sample_below(Scalar::<Secp256k1>::group_order());
		let rho = sample_relatively_prime_integer(ek_tilde.n.clone());
		let K = BigInt::mod_mul(
			&BigInt::mod_pow(&BigInt::add(&one, &ek_tilde.n), &k, &ek_tilde.nn),
			&BigInt::mod_pow(&rho, &ek_tilde.n, &ek_tilde.nn),
			&ek_tilde.nn,
		);

		(
			EncWitness { k, rho, phantom: PhantomData },
			EncCommonInput { N0: ek_tilde.n.clone(), NN0: ek_tilde.nn, K, phantom: PhantomData },
			EncSetupParameters { s, t, N_hat: ek_tilde.n, phantom: PhantomData },
		)
	}

	#[test]
	fn test_enc() {
		let (witness, common_input, setup_parameters) = generate_test_values();
		let proof =
			EncProof::<Secp256k1, Sha256>::prove(&witness, &common_input, &setup_parameters);
		assert!(
			EncProof::<Secp256k1, Sha256>::verify(&proof, &common_input, &setup_parameters).is_ok()
		);
	}
}
