use crate::refresh::rounds::{Round0, Round1, Round2};
use curv::elliptic::curves::Secp256k1;
use fs_dkr::{
	add_party_message::JoinMessage,
	error::{FsDkrError, FsDkrResult},
	refresh_message::RefreshMessage,
};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::LocalKey;
use private::InternalError;
use round_based::{
	containers::{
		push::{Push, PushExt},
		BroadcastMsgs, MessageStore, Store, StoreErr,
	},
	IsCritical, Msg, StateMachine,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{fmt, mem::replace, time::Duration};
use thiserror::Error;

pub struct KeyRefresh {
	// Current round
	round: R,

	// Messages
	round0_msgs: Option<Store<BroadcastMsgs<Option<JoinMessage>>>>,
	round1_msgs:
		Option<Store<BroadcastMsgs<Option<FsDkrResult<RefreshMessage<Secp256k1, Sha256>>>>>>,

	// Message queue
	msgs_queue: Vec<Msg<ProtocolMessage>>,

	party_i: u16,

	party_n: u16,
}

impl KeyRefresh {
	pub fn new(
		local_key_option: Option<LocalKey<Secp256k1>>,
		i: u16,
		t: u16,
		n: u16,
	) -> Result<Self> {
		if n < 2 {
			return Err(Error::TooFewParties)
		}
		if t == 0 || t >= n {
			return Err(Error::InvalidThreshold)
		}
		let mut state = Self {
			round: R::Round0(Round0 { local_key_option, t, n }),

			round0_msgs: Some(Round1::expects_messages(i, n)),
			round1_msgs: Some(Round2::expects_messages(i, n)),

			msgs_queue: vec![],

			party_i: i,

			party_n: n,
		};

		state.proceed_round(false)?;
		Ok(state)
	}

	fn gmap_queue<'a, T, F>(&'a mut self, mut f: F) -> impl Push<Msg<T>> + 'a
	where
		F: FnMut(T) -> M + 'a,
	{
		(&mut self.msgs_queue).gmap(move |m: Msg<T>| m.map_body(|m| ProtocolMessage(f(m))))
	}

	/// Proceeds round state if it received enough messages and if it's cheap to compute or
	/// `may_block == true`
	fn proceed_round(&mut self, may_block: bool) -> Result<()> {
		let store1_wants_more = self.round0_msgs.as_ref().map(|s| s.wants_more()).unwrap_or(false);
		let store2_wants_more = self.round1_msgs.as_ref().map(|s| s.wants_more()).unwrap_or(false);

		let next_state: R;

		let try_again: bool = match replace(&mut self.round, R::Gone) {
			R::Round0(round) if !round.is_expensive() || may_block => {
				next_state = round
					.proceed(self.gmap_queue(M::Round1))
					.map(R::Round1)
					.map_err(Error::ProceedRound)?;
				true
			},
			s @ R::Round0(_) => {
				next_state = s;
				false
			},
			R::Round1(round) if !store1_wants_more && (!round.is_expensive() || may_block) => {
				let store = self.round0_msgs.take().ok_or(InternalError::StoreGone)?;
				let msgs = store.finish().map_err(InternalError::RetrieveRoundMessages)?;
				next_state = round
					.proceed(msgs, self.gmap_queue(M::Round2))
					.map(R::Round2)
					.map_err(Error::ProceedRound)?;
				true
			},
			s @ R::Round1(_) => {
				next_state = s;
				false
			},
			R::Round2(round) if !store2_wants_more && (!round.is_expensive() || may_block) => {
				let store = self.round1_msgs.take().ok_or(InternalError::StoreGone)?;
				let msgs = store.finish().map_err(InternalError::RetrieveRoundMessages)?;
				next_state = round.proceed(msgs).map(R::Final).map_err(Error::ProceedRound)?;
				true
			},
			s @ R::Round2(_) => {
				next_state = s;
				false
			},
			s @ R::Final(_) | s @ R::Gone => {
				next_state = s;
				false
			},
		};
		self.round = next_state;
		if try_again {
			self.proceed_round(may_block)
		} else {
			Ok(())
		}
	}
}

impl StateMachine for KeyRefresh {
	type MessageBody = ProtocolMessage;
	type Err = Error;
	type Output = LocalKey<Secp256k1>;

	fn handle_incoming(&mut self, msg: Msg<Self::MessageBody>) -> Result<()> {
		let current_round = self.current_round();

		match msg.body {
			ProtocolMessage(M::Round1(m)) => {
				let store = self
					.round0_msgs
					.as_mut()
					.ok_or(Error::ReceivedOutOfOrderMessage { current_round, msg_round: 1 })?;
				store
					.push_msg(Msg { sender: msg.sender, receiver: msg.receiver, body: m })
					.map_err(Error::HandleMessage)?;
				self.proceed_round(false)
			},
			ProtocolMessage(M::Round2(m)) => {
				let store = self
					.round1_msgs
					.as_mut()
					.ok_or(Error::ReceivedOutOfOrderMessage { current_round, msg_round: 2 })?;
				store
					.push_msg(Msg { sender: msg.sender, receiver: msg.receiver, body: m })
					.map_err(Error::HandleMessage)?;
				self.proceed_round(false)
			},
		}
	}

	fn message_queue(&mut self) -> &mut Vec<Msg<Self::MessageBody>> {
		&mut self.msgs_queue
	}

	fn wants_to_proceed(&self) -> bool {
		let store1_wants_more = self.round0_msgs.as_ref().map(|s| s.wants_more()).unwrap_or(false);
		let store2_wants_more = self.round1_msgs.as_ref().map(|s| s.wants_more()).unwrap_or(false);

		match &self.round {
			R::Round0(_) => true,
			R::Round1(_) => !store1_wants_more,
			R::Round2(_) => !store2_wants_more,
			R::Final(_) | R::Gone => false,
		}
	}

	fn proceed(&mut self) -> Result<()> {
		self.proceed_round(true)
	}

	fn round_timeout(&self) -> Option<Duration> {
		None
	}

	fn round_timeout_reached(&mut self) -> Self::Err {
		panic!("no timeout was set")
	}

	fn is_finished(&self) -> bool {
		matches!(self.round, R::Final(_))
	}

	fn pick_output(&mut self) -> Option<Result<Self::Output>> {
		match self.round {
			R::Final(_) => (),
			R::Gone => return Some(Err(Error::DoublePickOutput)),
			_ => return None,
		}

		match replace(&mut self.round, R::Gone) {
			R::Final(result) => Some(Ok(result)),
			_ => unreachable!("guaranteed by match expression above"),
		}
	}

	fn current_round(&self) -> u16 {
		match &self.round {
			R::Round0(_) => 0,
			R::Round1(_) => 1,
			R::Round2(_) => 2,
			R::Final(_) | R::Gone => 3,
		}
	}

	fn total_rounds(&self) -> Option<u16> {
		Some(2)
	}

	fn party_ind(&self) -> u16 {
		self.party_i.into()
	}

	fn parties(&self) -> u16 {
		self.party_n.into()
	}
}

impl crate::traits::RoundBlame for KeyRefresh {
	fn round_blame(&self) -> (u16, Vec<u16>) {
		let store1_blame = self.round0_msgs.as_ref().map(|s| s.blame()).unwrap_or_default();
		let store2_blame = self.round1_msgs.as_ref().map(|s| s.blame()).unwrap_or_default();

		let default = (0, vec![]);
		match &self.round {
			R::Round0(_) => default,
			R::Round1(_) => store1_blame,
			R::Round2(_) => store2_blame,
			R::Final(_) | R::Gone => default,
		}
	}
}

impl fmt::Debug for KeyRefresh {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let current_round = match &self.round {
			R::Round0(_) => "0",
			R::Round1(_) => "1",
			R::Round2(_) => "2",
			R::Final(_) => "[Final]",
			R::Gone => "[Gone]",
		};
		let round0_msgs = match self.round0_msgs.as_ref() {
			Some(msgs) => format!("[{}/{}]", msgs.messages_received(), msgs.messages_total()),
			None => "[None]".into(),
		};
		let round1_msgs = match self.round1_msgs.as_ref() {
			Some(msgs) => format!("[{}/{}]", msgs.messages_received(), msgs.messages_total()),
			None => "[None]".into(),
		};
		write!(
			f,
			"{{Key refresh at round={} round0_msgs={} round1_msgs={} queue=[len={}]}}",
			current_round,
			round0_msgs,
			round1_msgs,
			self.msgs_queue.len()
		)
	}
}

// Rounds
enum R {
	Round0(Round0),
	Round1(Round1),
	Round2(Round2),
	Final(LocalKey<Secp256k1>),
	Gone,
}

// Messages

/// Protocol message which parties send on wire
///
/// Hides actual messages structure so it could be changed without breaking semver policy.
#[derive(Debug, Clone)]
pub struct ProtocolMessage(M);

#[derive(Debug, Clone)]
enum M {
	Round1(Option<JoinMessage>),
	Round2(Option<FsDkrResult<RefreshMessage<Secp256k1, Sha256>>>),
}

// Error

type Result<T> = std::result::Result<T, Error>;

/// Error type of key refresh protocol
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
	/// Round proceeding resulted in error
	#[error("proceed round: {0}")]
	ProceedRound(#[source] FsDkrError),

	/// Too few parties (`n < 2`)
	#[error("at least 2 parties are required for keygen")]
	TooFewParties,
	/// Threshold value `t` is not in range `[1; n-1]`
	#[error("threshold is not in range [1; n-1]")]
	InvalidThreshold,
	/// Party index `i` is not in range `[1; n]`
	#[error("party index is not in range [1; n]")]
	InvalidPartyIndex,

	/// Received message didn't pass pre-validation
	#[error("received message didn't pass pre-validation: {0}")]
	HandleMessage(#[source] StoreErr),
	/// Received message which we didn't expect to receive now (e.g. message from previous round)
	#[error(
		"didn't expect to receive message from round {msg_round} (being at round {current_round})"
	)]
	ReceivedOutOfOrderMessage { current_round: u16, msg_round: u16 },
	/// [Keygen::pick_output] called twice
	#[error("pick_output called twice")]
	DoublePickOutput,

	/// Some internal assertions were failed, which is a bug
	#[doc(hidden)]
	#[error("internal error: {0:?}")]
	InternalError(InternalError),
}

impl IsCritical for Error {
	fn is_critical(&self) -> bool {
		true
	}
}

impl From<InternalError> for Error {
	fn from(err: InternalError) -> Self {
		Self::InternalError(err)
	}
}

mod private {
	#[derive(Debug)]
	#[non_exhaustive]
	pub enum InternalError {
		/// [Messages store](super::MessageStore) reported that it received all messages it wanted
		/// to receive, but refused to return message container
		RetrieveRoundMessages(super::StoreErr),
		#[doc(hidden)]
		StoreGone,
	}
}

pub mod test {
    use curv::elliptic::curves::{Secp256k1};
    use round_based::dev::Simulation;
	use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2020::state_machine::keygen::*;
	use crate::refresh::state_machine::KeyRefresh;
	use curv::cryptographic_primitives::secret_sharing::feldman_vss::{
        ShamirSecretSharing, VerifiableSS,
    };

    fn simulate_keygen(t: u16, n: u16) -> Vec<LocalKey<Secp256k1>> {
        //simulate keygen
        let mut simulation = Simulation::new();
        simulation.enable_benchmarks(false);

        for i in 1..=n {
            simulation.add_party(Keygen::new(i, t, n).unwrap());
        }

        simulation.run().unwrap()
    }

	pub fn simulate_dkr_with_no_replacements(old_local_keys: Vec<LocalKey<Secp256k1>>) -> Vec<LocalKey<Secp256k1>> {
		let mut simulation = Simulation::new();
		simulation.enable_benchmarks(false);

		for old_local_key in old_local_keys {
			simulation.add_party(KeyRefresh::new(Some(old_local_key.clone()), old_local_key.clone().i, old_local_key.clone().t, old_local_key.n).unwrap());
		}

		 simulation.run().unwrap()
	}

	// Refresh Keys: Only Existing Parties (No New Parties)
	#[test]
	pub fn dkr_with_no_new_parties_test() {
		let t = 3;
		let n = 5;
		let local_keys = simulate_keygen(t, n);

		let mut old_local_keys = local_keys.clone();
		let mut new_local_keys = simulate_dkr_with_no_replacements(local_keys);

		// check that sum of old keys is equal to sum of new keys
		let old_linear_secret_key: Vec<_> = (0..old_local_keys.len())
		.map(|i| old_local_keys[i].keys_linear.x_i.clone())
		.collect();

		let new_linear_secret_key: Vec<_> =
			(0..new_local_keys.len()).map(|i| new_local_keys[i].keys_linear.x_i.clone()).collect();
		let indices: Vec<_> = (0..(t + 1)).collect();
		let vss = VerifiableSS::<Secp256k1> {
			parameters: ShamirSecretSharing {
				threshold: t,
				share_count: n,
			},
			commitments: Vec::new(),
		};
		assert_eq!(
			vss.reconstruct(&indices[..], &old_linear_secret_key[0..(t + 1) as usize]),
			vss.reconstruct(&indices[..], &new_linear_secret_key[0..(t + 1) as usize])
		);
		assert_ne!(old_linear_secret_key, new_linear_secret_key);
	}

	// Refresh Keys: All Existing Parties Stay, New Parties Join

	// Refresh Keys: Some Existing Parties Leave, New Parties Replace Them
}
