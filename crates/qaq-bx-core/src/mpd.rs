//! DASH → MPD manifest 生成(Windows 系统媒体引擎适配层)。
//!
//! 方案实证自 Richasy/Bili.Uwp(`DashVideoTemplate.xml` + NativePlayerViewModel):
//! B 站 playurl 的 DASH JSON 不是标准 MPD,系统 DASH 解析器吃不了;把选中的
//! video/audio Representation 填进 MPD 骨架后交给 `AdaptiveMediaSource`,系统
//! 管线自行拉两条 m4s 分轨。
//!
//! 关键约定(与 Bili.Uwp 一致,实测路径):
//! - 两个 Representation 的 `<BaseURL>` 留空,manifest URI 取视频流 base_url
//!   (分段相对解析 → 视频分段天然命中视频 URL);
//! - 音频分段由上层在 `DownloadRequested` 事件里按 `ResourceContentType ==
//!   "audio/mp4"` 重定向到音频 base_url(见 qaq-bx-video 的 native 引擎);
//! - `SegmentBase@initialization/indexRange` 原样内插(B 站 JSON 的范围串)。

use crate::player::{AudioStream, Dash, VideoStream};

/// MPD 骨架(双 AdaptationSet:video + audio)。
const MPD_TEMPLATE: &str = r#"<MPD xmlns="urn:mpeg:DASH:schema:MPD:2011" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" type="static">
    <Period start="PT0S">
        <AdaptationSet>
            <ContentComponent contentType="video" id="1" />
{video}
        </AdaptationSet>
        <AdaptationSet>
            <ContentComponent contentType="audio" id="2" />
{audio}
        </AdaptationSet>
    </Period>
</MPD>"#;

/// MPD 骨架(仅视频,音频外挂语义——本仓暂未用到,保留与 Bili.Uwp 对齐)。
#[allow(dead_code)]
const MPD_VIDEO_ONLY_TEMPLATE: &str = r#"<MPD xmlns="urn:mpeg:DASH:schema:MPD:2011" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" type="static">
    <Period start="PT0S">
        <AdaptationSet>
            <ContentComponent contentType="video" id="1" />
{video}
        </AdaptationSet>
    </Period>
</MPD>"#;

/// XML 文本转义(URL 里的 `&` 必须转,base_url 常见 query 参数)。
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn segment_base_xml(segment_base: Option<&crate::player::SegmentBase>) -> String {
    match segment_base {
        Some(sb) if !sb.index_range.is_empty() => format!(
            "<SegmentBase indexRange=\"{}\"><Initialization range=\"{}\" /></SegmentBase>",
            escape_xml(&sb.index_range),
            escape_xml(&sb.initialization)
        ),
        _ => String::new(),
    }
}

fn video_representation_xml(video: &VideoStream) -> String {
    format!(
        "<Representation bandwidth=\"{}\" codecs=\"{}\" height=\"{}\" mimeType=\"{}\" id=\"{}\" width=\"{}\" startWithSap=\"{}\">{}{}{}</Representation>",
        video.bandwidth,
        escape_xml(&video.codecs),
        video.height,
        escape_xml(if video.mime_type.is_empty() {
            "video/mp4"
        } else {
            &video.mime_type
        }),
        video.id,
        video.width,
        video.start_with_sap,
        "<BaseURL></BaseURL>",
        segment_base_xml(video.resolved_segment_base().as_ref()),
        ""
    )
}

fn audio_representation_xml(audio: &AudioStream) -> String {
    format!(
        "<Representation bandwidth=\"{}\" codecs=\"{}\" mimeType=\"{}\" id=\"{}\">{}{}{}</Representation>",
        audio.bandwidth,
        escape_xml(&audio.codecs),
        escape_xml(if audio.mime_type.is_empty() {
            "audio/mp4"
        } else {
            &audio.mime_type
        }),
        audio.id,
        "<BaseURL></BaseURL>",
        segment_base_xml(audio.resolved_segment_base().as_ref()),
        ""
    )
}

/// 由 DASH JSON + 选中分轨生成 MPD manifest 文本。
///
/// `video`/`audio` 通常来自 [`crate::player::select_stream_by_qn`] 的
/// `StreamChoice`;audio 传 `None` 生成仅视频 MPD(音频由上层外挂/重定向)。
pub fn build_mpd(video: &VideoStream, audio: Option<&AudioStream>) -> String {
    let video_xml = video_representation_xml(video);
    match audio {
        Some(audio) => MPD_TEMPLATE
            .replace("{video}", &video_xml)
            .replace("{audio}", &audio_representation_xml(audio)),
        None => MPD_VIDEO_ONLY_TEMPLATE.replace("{video}", &video_xml),
    }
}

/// 便捷封装:直接从整包 Dash 按清晰度选流生成 MPD(找不到对应流返回 None)。
pub fn build_mpd_from_dash(dash: &Dash, qn: i64, codec_priority: &[&str]) -> Option<String> {
    let choice = crate::player::select_stream_by_qn(dash, qn, codec_priority)?;
    Some(build_mpd(&choice.video, Some(&choice.audio)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::{SegmentBase, StreamChoice};

    fn sample_video() -> VideoStream {
        VideoStream {
            id: 80,
            codecs: "avc1.640032".into(),
            bandwidth: 2_000_000,
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            base_url: "https://upos.example.com/video.m4s?x=1&y=2".into(),
            backup_url: vec![],
            size: 123_456_789,
            mime_type: "video/mp4".into(),
            start_with_sap: 1,
            segment_base: Some(SegmentBase {
                initialization: "0-976".into(),
                index_range: "977-1721".into(),
            }),
            extra: Default::default(),
        }
    }

    fn sample_audio() -> AudioStream {
        AudioStream {
            id: 30280,
            codecs: "mp4a.40.2".into(),
            bandwidth: 320_000,
            base_url: "https://upos.example.com/audio.m4s".into(),
            backup_url: vec![],
            mime_type: "audio/mp4".into(),
            start_with_sap: 1,
            segment_base: Some(SegmentBase {
                initialization: "0-512".into(),
                index_range: "513-1024".into(),
            }),
            extra: Default::default(),
        }
    }

    #[test]
    fn build_mpd_contains_both_representations_with_ranges() {
        let mpd = build_mpd(&sample_video(), Some(&sample_audio()));
        assert!(mpd.starts_with("<MPD"));
        assert!(mpd.contains("urn:mpeg:dash:profile:isoff-on-demand:2011"));
        // 视频 Representation:属性齐全 + SegmentBase 原样内插。
        assert!(mpd.contains(r#"codecs="avc1.640032""#));
        assert!(mpd.contains(r#"mimeType="video/mp4""#));
        assert!(mpd.contains(r#"<SegmentBase indexRange="977-1721">"#));
        assert!(mpd.contains(r#"<Initialization range="0-976" />"#));
        // 音频 Representation。
        assert!(mpd.contains(r#"mimeType="audio/mp4""#));
        assert!(mpd.contains(r#"id="30280""#));
        // BaseURL 留空约定(音频由 DownloadRequested 重定向)。
        assert_eq!(mpd.matches("<BaseURL></BaseURL>").count(), 2);
        // 双 AdaptationSet。
        assert_eq!(mpd.matches("<AdaptationSet>").count(), 2);
    }

    #[test]
    fn build_mpd_escapes_xml_in_codecs_and_urls() {
        let mut video = sample_video();
        video.base_url = "https://upos.example.com/v.m4s?a=1&b=2".into();
        let mpd = build_mpd(&video, None);
        // 仅视频模板:无 audio AdaptationSet,且 URL 不进 MPD(BaseURL 留空),
        // codec 若含 & 需转义 —— 这里用 codecs 字段验证转义逻辑。
        let mut weird = sample_video();
        weird.codecs = "avc1&x<y".into();
        let mpd2 = build_mpd(&weird, None);
        assert!(mpd2.contains("avc1&amp;x&lt;y"));
        assert!(mpd.starts_with("<MPD"));
        assert_eq!(mpd.matches("<AdaptationSet>").count(), 1);
    }

    #[test]
    fn build_mpd_handles_missing_segment_base() {
        let mut video = sample_video();
        video.segment_base = None;
        let mpd = build_mpd(&video, None);
        assert!(!mpd.contains("SegmentBase"));
    }

    #[test]
    fn build_mpd_from_dash_selects_by_qn() {
        let dash = Dash {
            video: vec![sample_video()],
            audio: vec![sample_audio()],
        };
        let mpd = build_mpd_from_dash(&dash, 80, &["avc1"]).expect("qn=80 命中");
        assert!(mpd.contains(r#"id="80""#));
        assert!(build_mpd_from_dash(&dash, 120, &["avc1"]).is_none());
        let _: Option<StreamChoice> = None;
    }
}
