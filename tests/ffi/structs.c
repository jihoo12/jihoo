// C half of the struct-passing test (crates/jihoo-cli/tests/differential.rs,
// `native_structs_cross_into_c`): C functions taking and returning structs by
// value, of the shapes each C ABI treats differently, and a function calling
// back into the jihoo functions `structs.jh` exports.
#include <stdbool.h>
#include <stdint.h>
typedef struct { int32_t a, b; } I2;
typedef struct { double x, y; } D2;
typedef struct { float x, y, z; } F3;
typedef struct { int64_t a; double b; } ID;
typedef struct { uint8_t tag; float v; } BF;
typedef struct { int64_t a, b, c; } Big;
typedef struct { float v[4]; } F4;
typedef struct { double v[4]; } D4;
typedef struct { I2 inner; bool flag; } Nest;
typedef struct { uint16_t s; } Tiny;
typedef struct { int64_t a, b; } P2;

I2 i2_swap(I2 p) { return (I2){p.b, p.a}; }
D2 d2_scale(D2 p, double k) { return (D2){p.x * k, p.y * k}; }
F3 f3_add(F3 a, F3 b) { return (F3){a.x + b.x, a.y + b.y, a.z + b.z}; }
ID id_mix(ID p, int32_t n) { return (ID){p.a * n, p.b + n}; }
BF bf_next(BF p) { return (BF){(uint8_t)(p.tag + 1), p.v * 2}; }
Big big_sum(Big a, Big b) { return (Big){a.a + b.a, a.b + b.b, a.c + b.c}; }
F4 f4_rev(F4 p) { return (F4){{p.v[3], p.v[2], p.v[1], p.v[0]}}; }
D4 d4_twice(D4 p) { return (D4){{p.v[0] * 2, p.v[1] * 2, p.v[2] * 2, p.v[3] * 2}}; }
Nest nest_flip(Nest n) { return (Nest){{n.inner.b, n.inner.a}, !n.flag}; }
Tiny tiny_inc(Tiny t) { return (Tiny){(uint16_t)(t.s + 1)}; }
// Out of SSE registers: `e` goes on the stack, whole.
double spill(D2 a, D2 b, D2 c, D2 d, D2 e, double f) { return a.x + b.y + c.x + d.y + e.x * 100 + e.y * 1000 + f; }
// aarch64: `c` and `d` go on the stack, each at a multiple of 8.
float spill_f3(F3 a, F3 b, F3 c, F3 d) { return a.x + b.y + c.z * 10 + d.x * 100 + d.z * 1000; }
// x86_64: one general register is left, so `p` goes on the stack and `z` in it.
int64_t pair_last(I2 a, I2 b, I2 c, I2 d, I2 e, P2 p, int64_t z) { return a.a + e.b + p.a * 10 + p.b * 100 + z * 1000; }
// Out of general registers: `g` goes on the stack, whole, after `f`.
int64_t spill_int(I2 a, ID b, I2 c, I2 d, I2 e, ID f, I2 g) {
  return a.a + b.a + c.b + d.a + e.b + f.a + (int64_t)f.b + g.a * 1000 + g.b * 100000;
}

// The exported jihoo functions.
D2 jh_mid(D2 a, D2 b);
Big jh_big(Big a, int64_t k);
F3 jh_f3(F3 a);
int64_t jh_many(I2 a, I2 b, I2 c, I2 d, I2 e, I2 f, I2 g, Big h);
bool jh_odd(Tiny t);

int64_t call_back(void) {
  D2 m = jh_mid((D2){1, 2}, (D2){3, 6});
  if (m.x != 2 || m.y != 4) return 1;
  Big b = jh_big((Big){1, 2, 3}, 10);
  if (b.a != 10 || b.b != 20 || b.c != 30) return 2;
  F3 f = jh_f3((F3){1.5f, 2.5f, 3.5f});
  if (f.x != 3.5f || f.y != 2.5f || f.z != 1.5f) return 3;
  int64_t n = jh_many((I2){1, 2}, (I2){3, 4}, (I2){5, 6}, (I2){7, 8}, (I2){9, 10}, (I2){11, 12}, (I2){13, 14}, (Big){100, 200, 300});
  if (n != 66 + 600) return 4;
  if (!jh_odd((Tiny){7}) || jh_odd((Tiny){8})) return 5;
  return 0;
}
