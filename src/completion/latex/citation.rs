use super::combinators::{self, ArgumentContext, Parameter};
use crate::{
    completion::factory,
    feature::{FeatureProvider, FeatureRequest},
    protocol::{CompletionItem, CompletionParams, TextEdit},
    syntax::{bibtex, LANGUAGE_DATA},
    workspace::{Document, DocumentContent},
};
use futures_boxed::boxed;
use petgraph::graph::NodeIndex;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub struct LatexCitationCompletionProvider;

impl FeatureProvider for LatexCitationCompletionProvider {
    type Params = CompletionParams;
    type Output = Vec<CompletionItem>;

    #[boxed]
    async fn execute<'a>(&'a self, req: &'a FeatureRequest<Self::Params>) -> Self::Output {
        let parameters = LANGUAGE_DATA.citation_commands.iter().map(|cmd| Parameter {
            name: &cmd.name,
            index: cmd.index,
        });

        combinators::argument(req, parameters, |ctx| {
            async move {
                let mut items = Vec::new();
                for doc in req.related() {
                    if let DocumentContent::Bibtex(tree) = &doc.content {
                        for entry_node in tree.children(tree.root) {
                            if let Some(item) = Self::make_item(req, ctx, doc, tree, entry_node) {
                                items.push(item);
                            }
                        }
                    }
                }
                items
            }
        })
        .await
    }
}

impl LatexCitationCompletionProvider {
    fn make_item(
        req: &FeatureRequest<CompletionParams>,
        ctx: ArgumentContext,
        doc: &Document,
        tree: &bibtex::Tree,
        entry_node: NodeIndex,
    ) -> Option<CompletionItem> {
        let entry = tree.as_entry(entry_node)?;
        if entry.is_comment() {
            return None;
        }

        let key = entry.key.as_ref()?.text().to_owned();
        let text_edit = TextEdit::new(ctx.range, key.clone());
        let item = factory::citation(req, doc.uri.clone(), tree, entry_node, key, text_edit);
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        feature::FeatureTester,
        protocol::{Range, RangeExt},
    };
    use indoc::indoc;

    #[tokio::test]
    async fn empty_latex_document() {
        let actual_items = FeatureTester::new()
            .file("main.tex", "")
            .main("main.tex")
            .position(0, 0)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert!(actual_items.is_empty());
    }

    #[tokio::test]
    async fn empty_bibtex_document() {
        let actual_items = FeatureTester::new()
            .file("main.bib", "")
            .main("main.bib")
            .position(0, 0)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert!(actual_items.is_empty());
    }

    #[tokio::test]
    async fn incomplete() {
        let actual_items = FeatureTester::new()
            .file(
                "main.tex",
                indoc!(
                    r#"
                        \addbibresource{main.bib}
                        \cite{
                        \begin{foo}
                        \end{bar}
                    "#
                ),
            )
            .file("main.bib", "@article{foo,}")
            .main("main.tex")
            .position(1, 6)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert_eq!(actual_items.len(), 1);
        assert_eq!(actual_items[0].label, "foo");
        assert_eq!(
            actual_items[0]
                .text_edit
                .as_ref()
                .map(|edit| edit.range)
                .unwrap(),
            Range::new_simple(1, 6, 1, 6)
        );
    }

    #[tokio::test]
    async fn empty_key() {
        let actual_items = FeatureTester::new()
            .file(
                "foo.tex",
                indoc!(
                    r#"
                        \addbibresource{bar.bib}
                        \cite{}  
                    "#
                ),
            )
            .file("bar.bib", "@article{foo,}")
            .file("baz.bib", "@article{bar,}")
            .main("foo.tex")
            .position(1, 6)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert_eq!(actual_items.len(), 1);
        assert_eq!(actual_items[0].label, "foo");
        assert_eq!(
            actual_items[0]
                .text_edit
                .as_ref()
                .map(|edit| edit.range)
                .unwrap(),
            Range::new_simple(1, 6, 1, 6)
        );
    }

    #[tokio::test]
    async fn single_key() {
        let actual_items = FeatureTester::new()
            .file(
                "foo.tex",
                indoc!(
                    r#"
                    \addbibresource{bar.bib}
                    \cite{foo}  
                "#
                ),
            )
            .file("bar.bib", "@article{foo,}")
            .file("baz.bib", "@article{bar,}")
            .main("foo.tex")
            .position(1, 6)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert_eq!(actual_items.len(), 1);
        assert_eq!(actual_items[0].label, "foo");
        assert_eq!(
            actual_items[0]
                .text_edit
                .as_ref()
                .map(|edit| edit.range)
                .unwrap(),
            Range::new_simple(1, 6, 1, 9)
        );
    }

    #[tokio::test]
    async fn second_key() {
        let actual_items = FeatureTester::new()
            .file(
                "foo.tex",
                indoc!(
                    r#"
                    \addbibresource{bar.bib}
                    \cite{foo,}  
                "#
                ),
            )
            .file("bar.bib", "@article{foo,}")
            .file("baz.bib", "@article{bar,}")
            .main("foo.tex")
            .position(1, 10)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert_eq!(actual_items.len(), 1);
        assert_eq!(actual_items[0].label, "foo");
        assert_eq!(
            actual_items[0]
                .text_edit
                .as_ref()
                .map(|edit| edit.range)
                .unwrap(),
            Range::new_simple(1, 10, 1, 10)
        );
    }

    #[tokio::test]
    async fn outside_cite() {
        let actual_items = FeatureTester::new()
            .file(
                "foo.tex",
                indoc!(
                    r#"
                        \addbibresource{bar.bib}
                        \cite{}  
                    "#
                ),
            )
            .file("bar.bib", "@article{foo,}")
            .file("baz.bib", "@article{bar,}")
            .main("foo.tex")
            .position(1, 7)
            .test_completion(LatexCitationCompletionProvider)
            .await;

        assert!(actual_items.is_empty());
    }
}
