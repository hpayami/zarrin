; ModuleID = 'zarrin'
source_filename = "zarrin"

@strlit = global [5 x i8] c"%ld\0A\00"
@strlit.1 = global [5 x i8] c"%ld\0A\00"
@strlit.2 = global [18 x i8] c"hello from zarrin\00"
@strlit.3 = global [4 x i8] c"%s\0A\00"

declare i64 @printf(ptr, ...)

define i64 @square(i64 %0) {
entry:
  %x = alloca i64, align 8
  store i64 %0, ptr %x, align 4
  %x1 = load i64, ptr %x, align 4
  %x2 = load i64, ptr %x, align 4
  %mul = mul i64 %x1, %x2
  ret i64 %mul
}

define i64 @main() {
entry:
  %a = alloca i64, align 8
  store i64 3, ptr %a, align 4
  %b = alloca i64, align 8
  store i64 4, ptr %b, align 4
  %a1 = load i64, ptr %a, align 4
  %square = call i64 @square(i64 %a1)
  %s = alloca i64, align 8
  store i64 %square, ptr %s, align 4
  %s2 = load i64, ptr %s, align 4
  %call_printf = call i64 (ptr, ...) @printf(ptr @strlit, i64 %s2)
  %a3 = load i64, ptr %a, align 4
  %b4 = load i64, ptr %b, align 4
  %add = add i64 %a3, %b4
  %call_printf5 = call i64 (ptr, ...) @printf(ptr @strlit.1, i64 %add)
  %call_printf6 = call i64 (ptr, ...) @printf(ptr @strlit.3, ptr @strlit.2)
  ret i64 0
}
