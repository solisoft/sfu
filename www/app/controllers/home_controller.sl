# Health endpoint (the docs pages live in docs_controller.sl).

class HomeController < Controller
    # GET /health
    def health
        return render_json({ "status": "ok" })
    end
end
