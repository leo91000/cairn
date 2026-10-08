use leo_relay_protocol::{
    Frame, MAX_FRAME, Role,
    data_channel::{EncodedFrame, FrameDecoder, ReassemblyBudget},
};

fn fragment(id: u32, total: usize, offset: usize, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![1];
    packet.extend(id.to_be_bytes());
    packet.extend((total as u32).to_be_bytes());
    packet.extend((offset as u32).to_be_bytes());
    packet.extend(payload);
    packet
}

#[test]
fn a_trickling_member_cannot_hold_small_frames_from_other_members_or_the_owner() {
    let budget = ReassemblyBudget::default();
    let mut holder = FrameDecoder::with_budget(budget.for_account("slow", Role::Member));
    let mut member = FrameDecoder::with_budget(budget.for_account("other", Role::Member));
    let mut owner = FrameDecoder::with_budget(budget.for_account("owner", Role::Owner));
    let encoded = EncodedFrame::new(2, &Frame::Cancel { id: "small".into() }).unwrap();
    let packet = encoded.packets().next().unwrap();

    assert!(
        holder
            .push(&fragment(1, MAX_FRAME, 0, b"{"))
            .unwrap()
            .is_none()
    );
    for offset in 1..5 {
        assert!(
            holder
                .push(&fragment(1, MAX_FRAME, offset, b" "))
                .unwrap()
                .is_none()
        );
        for decoder in [&mut member, &mut owner, &mut holder] {
            assert!(
                matches!(decoder.push(&packet), Ok(Some(Frame::Cancel { id })) if id == "small")
            );
        }
    }
}

#[test]
fn a_busy_member_upload_is_rejected_and_drained_before_its_next_small_frame() {
    use leo_relay_protocol::{ApiRequest, data_channel::DecodeError};
    let budget = ReassemblyBudget::default();
    let mut holder = FrameDecoder::with_budget(budget.for_account("slow", Role::Member));
    let mut member = FrameDecoder::with_budget(budget.for_account("other", Role::Member));
    holder.push(&fragment(1, MAX_FRAME, 0, b"{")).unwrap();
    let upload = Frame::Request(ApiRequest {
        id: "upload".into(),
        account_id: "other".into(),
        role: Role::Member,
        mcp_scopes: None,
        public_artifact: None,
        method: "POST".into(),
        path: "/api/chats".into(),
        headers: Vec::new(),
        body: vec![0; 20_000],
    });
    let encoded = EncodedFrame::new(3, &upload).unwrap();
    let mut packets = encoded.packets();
    assert!(
        matches!(member.push(&packets.next().unwrap()), Err(DecodeError::Rejected(Some(id))) if id == "upload")
    );
    for packet in packets {
        assert!(member.push(&packet).unwrap().is_none());
    }
    let small = EncodedFrame::new(4, &Frame::Cancel { id: "small".into() }).unwrap();
    assert!(
        matches!(member.push(&small.packets().next().unwrap()), Ok(Some(Frame::Cancel { id })) if id == "small")
    );
}
