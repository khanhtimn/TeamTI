use domain::media::PlayableSource;
use domain::error::DomainError;
use songbird::input::Input;

pub async fn map_playable_to_songbird(source: PlayableSource) -> Result<Input, DomainError> {
    match source {
        PlayableSource::ResolvedPlayable { path, .. } => {
            let src = songbird::input::File::new(path);
            Ok(src.into())
        }
        PlayableSource::UnresolvedRemote(_) => {
            Err(DomainError::InvalidState("Remote sources not supported in v1".into()))
        }
    }
}
