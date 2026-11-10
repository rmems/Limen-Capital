include(joinpath(@__DIR__, "..", "compare_model_math.jl"))

@testset "Fixed-seed before/after readout replay" begin
    threads = LinearAlgebra.BLAS.get_num_threads()
    LinearAlgebra.BLAS.set_num_threads(1)
    try
        withenv("LIMEN_AGG_MODE" => "relevance") do
            result = ModelMathComparison.comparison()
            @test result == ModelMathComparison.comparison()
            @test result.old_decreases == 0
            @test result.new_decreases > 0
            @test result.old_momentum == 0
            @test result.new_momentum > 0
            @test result.old_saturation == 1
            @test result.new_saturation < result.old_saturation
            @test all(==(first(result.old_final)), result.old_final)
            @test length(unique(result.new_final)) > 1
            @test result.mean_abs_difference > 0
        end
    finally
        LinearAlgebra.BLAS.set_num_threads(threads)
    end
end
