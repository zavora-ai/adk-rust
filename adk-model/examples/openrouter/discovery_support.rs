pub fn model_author_slug(model: &str) -> Option<(String, String)> {
    model.split_once('/').map(|(author, slug)| (author.to_string(), slug.to_string()))
}
