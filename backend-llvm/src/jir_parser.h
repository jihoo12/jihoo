#pragma once

#include <stdexcept>
#include <string>

#include "jir.h"

namespace jir {

struct ParseError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

// Parses the JIR text format. Throws ParseError with "line N: ..." on bad input.
Module parse(const std::string &text);

}  // namespace jir
