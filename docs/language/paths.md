# Paths

A path is a reference into a value that holds no address: the way from a
struct to one of its parts, with the indices of the elements on the way. It
can be stored, returned, sent and compared like a number, and it can never
dangle, because it points into nothing until it is followed from a value.

```jihoo
struct Node {
    weight: i64
    edges: [path(Graph).nodes[_]]   // edges to other nodes of the same graph
}

struct Graph {
    nodes: [Node]
}

fn heaviest_neighbour(g: Graph, n: path(Graph).nodes[_]) -> i64 {
    let best = 0
    let i = 0
    while i < len(g[n].edges) {
        let w = g[g[n].edges[i]].weight      // follow a path found through a path
        if w > best {
            best = w
        }
        i = i + 1
    }
    return best
}
```

Graphs, doubly linked lists and trees with parent links need parts that refer
to each other. With pointers that is where memory errors come from; with
`ref` it needs the GC. Paths do it in every profile, at the cost of an index.

## Path types

`path(Root).a[_].b` is the type of a path from a `Root`, a struct, through
field `a`, an element of it (an array or list), and field `b` of that element.
Each `[_]` is an index the path value holds; fields cost nothing.

A path goes through struct fields and the elements of arrays and lists only:
never through a pointer, `ref` or `cell`, so it always stays inside the value
it is followed from.

## Making paths

- `path(Root)` is the path from a `Root` to itself.
- `p.f` is `p` followed by field `f`, and `p[i]` is `p` followed by element
  `i`: `path(Graph).nodes[3]`, or `p.edges[0]` from a `p` that leads to a node.
- `path_index(p)` is the index of the last element step of `p`, and
  `path_index(p, k)` that of step `k`, counting from 0.
- `==` and `!=` compare paths of the same type by their indices.

## Following paths

`root[p]`, where `p` is a path from the type of `root`, is the part `p` leads
to, as if the path were written out: with `p = path(Graph).nodes[i]`,
`g[p].weight` is `g.nodes[i].weight`. It can be read and assigned, `push` and
`pop` work on it, and it compiles to the same code. `root` can be a variable
or a part of one, what a `cell` holds, what a `ref` refers to, or what a
pointer `*Root` points to.

Elements are bounds-checked as usual, so a path whose element was removed
stops the program, or leads to whatever element is now at that index. That
is a bug in the program, not a memory error: a path never reaches memory the
value does not own.
