//! Real packets from capture 5: a Paper server with Simple Voice Chat (compat 20),
//! one PC client on loopback and a second player (on another device) whose voice
//! the server relayed to the PC. Constants generated from the capture.

use horizon_voicechat_net::tcp::{self, Group, JoinedGroup, PlayerState, Secret};
use horizon_voicechat_net::udp::{self, Cipher, Packet};
use horizon_voicechat_net::Uuid;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

const SECRET_PAYLOAD: &str = "0c741dce19b10e134b7cc93facf5282500005f86936a93914e253635bbc48eae6b883b2900000004fb4048000000000000000003e8010001";
const STATES: &str = "0000000200001efedefe0a5b374d92051eb799151a8105356875746d000001936a93914e253635bbc48eae6b883b2905356875745f00";
const STATE_CONNECTING: &str = "0001936a93914e253635bbc48eae6b883b2905356875745f00";
const STATE_CONNECTED: &str = "0000936a93914e253635bbc48eae6b883b2905356875745f00";
const ADD_GROUP: &str = "40180f619bd44f4c86a42e4b7ff394a9033131310100000000";
const STATE_IN_GROUP: &str = "0000936a93914e253635bbc48eae6b883b2905356875745f0140180f619bd44f4c86a42e4b7ff394a9";
const JOINED_GROUP: &str = "0140180f619bd44f4c86a42e4b7ff394a900";
const STATE_OTHER_IN_GROUP: &str = "00001efedefe0a5b374d92051eb799151a8105356875746d0140180f619bd44f4c86a42e4b7ff394a9";
const CREATE_GROUP_FROM_CLIENT: &str = "0331313101033131310000";
const UPDATE_STATE_FROM_CLIENT: &str = "00";

const SOUND_DATAGRAM: &str = "ff9c028cadbffc141089a70e08d329949f18c20feb7a6ce6441fc21bc29afb57f10366804ee813c9edf36814670dbac182fc13bc14ce3ec9d8c680e4a821854836c5dee11da731c3f6aa349503c8b272154686883bfd78f5a12950905552733b0ea59615f7a1047e5ac4ac13cecd0db23534172c1170d2da0645c0daebbca3509895fff67586735b0e8650280aeb9507064437c62e492c040294218ee14632018f8c6623a5b79b0782c461759060fb89c2dfbdd7b32c52389a52b7e1b257095201bf00824691fc81ea5d93cebb5288c28e338493cc19aa6f91a10f94d56fd78e153ebc27199d3d3bed94ab596ec765197d652d411cc334ee0a13a0cb73c64cb7e99e2abca584abe7d74df0787b5943ba11bf1d48f7f89bb10a7de4c7aceb3f";
const SOUND_BIG_DATAGRAM: &str = "ff9c04c7d46959e245b43e5042e293fcea7dfc6eb196ed767b66be434f2bd2663c1f715144f01a190622755101d7a52dc00236623013e4d44f04e1f795e42814328ea246ad01ad8e214e8d1345c7ef3d0972e7b0f7dcf6e95c31d2902f6dd66e32a76ace2e09be8243451714310e510e09ee4c450d7599a0ef137a44a0c618cb3265dfcacf8e6bc70b0581d970d0fd7acd1acf8face04a4ebd5aa8b7665ab2a71264b3196283327ec4793c950c6517f4b93b780482e4840a3179eebf9e3ab21e4c3d5940e5c443adf4c8699ce2e7126cbae384466697da950efa56eb2edc4568a9c83e72fbbe9a8ff767d5cd985726d724cab360c4d6cc3139ec2165e3b27e1f721b68d125727a62a9ff893dc71b3931f98b71f18c4425f2a36b04c21615ee3a6adff943c4d5cfcf0c4a2f774696ab609424b04345646bb8d3e550d744f2f2583c60482508d5790b487cb985b62e8cc91301d1dc23a4aaabe0a5bafbff7ef8aacfba48af3e7d418802a7460e2eb30c4cfa26cec35ff206279d3366ee3b2bffddccc30157a611f17864c21c6bab763058102df9d66cf0600c310c02c8f1b35d560e2111812aa8e6c9a9cee0c8c041e363a88747abe8644c4c4df544535b7cdb8c81fc7a60bcabd331200eee4658afb1dc75cae92ca1cc4c8a1fe4c3774651290d896fac51222bc3839204a393788747460ff579e77bc763487261dcc0de1898040f5fcc3bf66d5c05331fb02dd3123cd47b31844e122666c27b12b894fd81ae";
const GROUP_SOUND_DATAGRAM: &str = "ff9c0270deae9b47e390b0916cac853e93b48fc9ab0a080e8d16034df0078ebd5b473a4046ecbf2babb564fcb55a4fdadef1ba718182cdba818e3195c69127210ed40e67fd5d8cdc3ce934c5343143e13b946e04768871ca0db872ff16c8ec9b583427fd78d987570e75d7e177217da6d6008a20912435aa7c6ab05c2bb98f00a216c96d9fb87e131d5cbaa83f7be66be0df4ee8aecdf1b8b0e26f486d20c815030eba9357b763e7acda1c09a9eb6718da9f84c3d9245b8a0db840d510a1457043c1e66ce05b2b5d232201f72f01c13478bf5da3c8e9664a138cd6cd6a98008d8af79b207c5ec2aacba410821aeddd5f57a05261bdc05279721c3e8551719a11bea35a36a49bba2b5ef0e6cdf375a5e3ef51b7db524c23fd7beb060fc3b83e";
const GROUP_SOUND_END_DATAGRAM: &str = "ff9c02281739344128ef35c21afbeeac3c46642079bae7bcd603aaff6867bb91a16b1428460e0688d9d493c4008345c7bb3ff11b2809eceef93dde8706c0ea2ce58876e997cb3cf4cb11a4977dff5892ba5c1982bcb33be34fceb2ff4e7c85358f9bed727a020ba278480a5bd0b77900cf3a2fe9aaa66094591151aeac50b320cdb9967b6108e34a903a1963f87651e56bec32c18899f6a2a51a213ad91f35c9ebb05a77af627735f17509ad3da3ee47cedc2262a2253bc18ca52404136aaeaacb0b0ee0de9c684bd326c4e62ffb47f417553e93ec600ec6fc09e13a5f470f9a1d082e201c990baf596f861f89659a9d0f6c38b95c24c44c4dc36de2bf6fcb79ce2e94b5cf5aaeeeabb209dee4d3bb2a441b6b574a7b35f0769a187463ba60";
const MIC_END_DATAGRAM: &str = "ff936a93914e253635bbc48eae6b883b29270f128a1daa0c1fbe6e074d3cb33325ecbb8338f5ca6332b984c91b891c178734a97ca9d5defaff";

fn uuid(s: &str) -> Uuid {
    Uuid::from_hex(s).unwrap()
}

fn me() -> Uuid {
    uuid("936a9391-4e25-3635-bbc4-8eae6b883b29")
}

fn other() -> Uuid {
    uuid("1efedefe-0a5b-374d-9205-1eb799151a81")
}

fn group_id() -> Uuid {
    uuid("40180f61-9bd4-4f4c-86a4-2e4b7ff394a9")
}

fn cipher() -> Cipher {
    Cipher::new(&Secret::parse(&unhex(SECRET_PAYLOAD)).unwrap().secret)
}

#[test]
fn second_secret_payload_parses() {
    let s = Secret::parse(&unhex(SECRET_PAYLOAD)).unwrap();
    assert_eq!(s.secret, uuid("0c741dce-19b1-0e13-4b7c-c93facf52825"));
    assert_eq!(s.player, me());
    assert_eq!((s.server_port, s.keep_alive_ms, s.codec), (24454, 1000, 0));
}

#[test]
fn states_lists_both_players() {
    let states = tcp::parse_states(&unhex(STATES)).unwrap();
    assert_eq!(
        states,
        vec![
            PlayerState { disabled: false, disconnected: false, player: other(), name: "5hutm".into(), group: None },
            PlayerState { disabled: false, disconnected: true, player: me(), name: "5hut_".into(), group: None },
        ]
    );
}

#[test]
fn state_flips_to_connected_and_tracks_groups() {
    let connecting = PlayerState::parse(&unhex(STATE_CONNECTING)).unwrap();
    let connected = PlayerState::parse(&unhex(STATE_CONNECTED)).unwrap();
    assert!(connecting.disconnected && !connected.disconnected);
    assert_eq!(connected.group, None);

    let in_group = PlayerState::parse(&unhex(STATE_IN_GROUP)).unwrap();
    assert_eq!((in_group.player, in_group.group), (me(), Some(group_id())));
    let other_in_group = PlayerState::parse(&unhex(STATE_OTHER_IN_GROUP)).unwrap();
    assert_eq!((other_in_group.name.as_str(), other_in_group.group), ("5hutm", Some(group_id())));
}

#[test]
fn group_payloads() {
    let g = Group::parse(&unhex(ADD_GROUP)).unwrap();
    assert_eq!(g, Group { id: group_id(), name: "111".into(), has_password: true, trailing: [0; 4] });
    let j = JoinedGroup::parse(&unhex(JOINED_GROUP)).unwrap();
    assert_eq!(j, JoinedGroup { in_group: true, group: group_id(), wrong_password: false });
}

#[test]
fn client_payloads_match_what_the_original_client_sent() {
    assert_eq!(tcp::create_group("111", Some("111")), unhex(CREATE_GROUP_FROM_CLIENT));
    assert_eq!(tcp::update_state(false), unhex(UPDATE_STATE_FROM_CLIENT));
}

#[test]
fn truncated_state_payloads_are_errors_not_panics() {
    for hex in [STATES, STATE_IN_GROUP, ADD_GROUP, JOINED_GROUP] {
        let bytes = unhex(hex);
        for cut in 0..bytes.len() {
            let _ = tcp::parse_states(&bytes[..cut]);
            let _ = PlayerState::parse(&bytes[..cut]);
            let _ = Group::parse(&bytes[..cut]);
            let _ = JoinedGroup::parse(&bytes[..cut]);
        }
    }
    // A hostile count must neither allocate wildly nor panic.
    assert!(tcp::parse_states(&[0x7f, 0xff, 0xff, 0xff]).is_err());
    assert!(tcp::parse_states(&[0xff, 0xff, 0xff, 0xff]).is_err());
}

fn open_down(hex: &str) -> (Vec<u8>, Packet) {
    let datagram = unhex(hex);
    let payload = udp::unframe_from_server(&datagram).unwrap();
    let plain = cipher().open(payload).unwrap();
    let packet = Packet::decode(&plain).unwrap();
    (plain, packet)
}

#[test]
fn proximity_voice_from_another_player() {
    let (plain, packet) = open_down(SOUND_DATAGRAM);
    assert_eq!(plain.len(), 256);
    match &packet {
        Packet::PlayerSound { first_id, second_id, opus, sequence, distance } => {
            assert_eq!((*first_id, *second_id), (other(), other()));
            assert_eq!(opus.len(), 34);
            assert_eq!(*sequence, 231);
            assert_eq!(*distance, 48.0);
        }
        other => panic!("expected PlayerSound, got {other:?}"),
    }
    // Our encoding is the original's plaintext minus the server's padding.
    let unpadded = packet.encode();
    assert_eq!(&plain[..unpadded.len()], &unpadded[..]);
    assert!(plain[unpadded.len()..].iter().all(|b| *b == 0));
}

#[test]
fn server_pads_to_a_multiple_of_256_not_always_256() {
    let (plain, packet) = open_down(SOUND_BIG_DATAGRAM);
    assert_eq!(plain.len(), 512);
    match packet {
        Packet::PlayerSound { opus, sequence, .. } => {
            assert_eq!(opus.len(), 222);
            assert_eq!(sequence, 2532);
        }
        other => panic!("expected PlayerSound, got {other:?}"),
    }
}

#[test]
fn group_voice_and_the_empty_frame_that_ends_a_transmission() {
    let (_, first) = open_down(GROUP_SOUND_DATAGRAM);
    match first {
        Packet::GroupSound { first_id, second_id, opus, sequence } => {
            assert_eq!((first_id, second_id), (other(), other()));
            assert!(!opus.is_empty());
            assert_eq!(sequence, 2746);
        }
        other => panic!("expected GroupSound, got {other:?}"),
    }
    let (_, end) = open_down(GROUP_SOUND_END_DATAGRAM);
    assert_eq!(end, Packet::GroupSound { first_id: other(), second_id: other(), opus: vec![], sequence: 3203 });
}

#[test]
fn our_microphone_end_marker_is_what_the_original_client_sent() {
    let datagram = unhex(MIC_END_DATAGRAM);
    let (player, payload) = udp::unframe_to_server(&datagram).unwrap();
    assert_eq!(player, me());
    let plain = cipher().open(payload).unwrap();
    let packet = Packet::decode(&plain).unwrap();
    assert_eq!(packet, Packet::Mic { opus: vec![], sequence: 26, flags: 0 });
    assert_eq!(packet.encode(), plain);
}
