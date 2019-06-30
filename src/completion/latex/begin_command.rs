use crate::completion::factory::{self, LatexComponentId};
use crate::completion::latex::combinators;
use crate::feature::{FeatureProvider, FeatureRequest};
use futures_boxed::boxed;
use lsp_types::{CompletionItem, CompletionParams};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct LatexBeginCommandCompletionProvider;

impl FeatureProvider for LatexBeginCommandCompletionProvider {
    type Params = CompletionParams;
    type Output = Vec<CompletionItem>;

    #[boxed]
    async fn execute<'a>(&'a self, request: &'a FeatureRequest<Self::Params>) -> Self::Output {
        combinators::command(request, async move |_| {
            let snippet = factory::command_snippet(
                request,
                "begin",
                "begin{$1}\n\t$0\n\\end{$1}",
                &LatexComponentId::Kernel,
            );
            vec![snippet]
        })
        .await
    }
}