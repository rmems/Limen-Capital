using LimenCapital

output = isempty(ARGS) ? joinpath(@__DIR__, "..", "runs", "rm-1865") : abspath(ARGS[1])
summary = run_comparison(output; data_path=length(ARGS)>1 ? abspath(ARGS[2]) : nothing)
println("Comparison artifacts: ", output)
failed = count(o -> o.status != "ok", summary.outcomes)
println("Outcomes: ", length(summary.outcomes), "; failed: ", failed)
failed == 0 || exit(1)
