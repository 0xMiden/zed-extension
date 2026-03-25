(procedure
  body: (block) @function.inside) @function.around

(entrypoint
  body: (block) @function.inside) @function.around

(doc_comment
  (doc_comment_line)+ @comment.inside) @comment.around

(comment) @comment.around
